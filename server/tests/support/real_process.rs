//! The real binary under a test: a scratch site (key file made by the binary's own `keygen`, a certificate, a configuration), the
//! process with its standard output and standard error captured line by line, a TLS client with a cookie jar that sends what the
//! page sends, the enrolment driver, and a socket client that records every message it reads.
//!
//! The process runs the in-process worker mode, so the test binary is built with the `in-process-workers` feature; the path
//! of the spawn helper (the product's worker launcher) is left to the isolation and image tests.
//!
//! Tests pull this file in with `#[path]` after `edge.rs`, `edge_tls.rs` and `ws_env.rs`, which it uses through `crate::`.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use holzwave_server::config::{ENV_CONFIG, ENV_KEY_FILE};
use holzwave_server::http::health::{self, Health};
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};

use crate::edge::{HOST, ORIGIN};
use crate::edge_tls::{Conn, Reply, Request};
use crate::ws_env::scratch::Scratch;
use crate::ws_env::{Got, Incoming, WsClient, tls_to};

/// The binary under test, built with the same feature set as the test.
pub const BINARY: &str = env!("CARGO_BIN_EXE_holzwave-server");

/// How long a start may take before it counts as failed.
pub const START_WITHIN: Duration = Duration::from_secs(30);

/// The password that passes the policy, for people the suites make.
pub const PASSWORD: &str = "correct horse battery";

// ---- the site ----------------------------------------------------------------------------------------------------------------

/// A scratch directory with the key file, the certificate and the configuration of one server.
pub struct Site {
    pub scratch: Scratch,
    pub run: Scratch,
    pub config: Value,
    pub key: PathBuf,
    pub der: CertificateDer<'static>,
    pub port: u16,
}

/// A free loopback port. The window between this call and the server's bind is the usual one of such tests.
pub fn free_port() -> u16 {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    socket.local_addr().expect("address").port()
}

impl Site {
    /// A site whose key file was made by `holzwave-server keygen`, with the password-and-code method on. A client secret file
    /// with `oidc_secret` as its content adds one single sign-on provider.
    pub fn new(oidc_secret: Option<&str>) -> Site {
        let scratch = Scratch::new("proc");
        let out_path = scratch.join("server.key");
        let made = Command::new(BINARY)
            .args(["keygen", "--out"])
            .arg(&out_path)
            .env_clear()
            .output()
            .expect("the key generator runs");
        assert_eq!(made.status.code(), Some(0), "keygen: {}", String::from_utf8_lossy(&made.stderr));
        let cert_made = rcgen::generate_simple_self_signed(vec![HOST.to_owned()]).expect("a certificate");
        let cert = scratch.write_file("cert.pem", cert_made.cert.pem().as_bytes(), 0o644);
        let cert_key = scratch.write_file("cert-key.pem", cert_made.signing_key.serialize_pem().as_bytes(), 0o600);
        let data = scratch.join("data");
        std::fs::DirBuilder::new().mode(0o700).create(&data).expect("a private data directory");
        let run = Scratch::short("r");
        let port = free_port();
        let mut config = json!({
            "public_url": ORIGIN,
            "listen": format!("127.0.0.1:{port}"),
            "data_dir": data,
            "runtime_dir": run.path(),
            "tls": {"mode": "files", "cert_file": cert, "key_file": cert_key},
            "auth": {"passkey": true, "password_totp": true},
            "workers": {"mode": "in_process", "uid_min": 20000, "uid_max": 29999}
        });
        if let Some(secret) = oidc_secret {
            let file = scratch.write_file("oidc-secret", format!("{secret}\n").as_bytes(), 0o600);
            config["auth"]["oidc"] = json!([{
                "id": "corp", "name": "Company", "issuer": "https://idp.example.test", "client_id": "hw-client",
                "client_secret_file": file, "allowed_domains": ["example.org"]
            }]);
        }
        Site { scratch, run, config, key: out_path, der: cert_made.cert.der().clone(), port }
    }

    /// Sets a member by a dotted path, making the sections on the way.
    pub fn set(&mut self, path: &str, value: Value) {
        let mut at = &mut self.config;
        let parts: Vec<&str> = path.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            at = at.as_object_mut().expect("an object").entry(*part).or_insert_with(|| json!({}));
        }
        at.as_object_mut().expect("an object").insert(parts[parts.len() - 1].to_owned(), value);
    }

    /// Removes a member by a dotted path.
    pub fn unset(&mut self, path: &str) {
        let mut at = &mut self.config;
        let parts: Vec<&str> = path.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            at = at.get_mut(*part).expect("a section");
        }
        at.as_object_mut().expect("an object").remove(parts[parts.len() - 1]);
    }

    /// Writes the configuration to a new file (the earlier one is replaced) and answers its path.
    pub fn config_path(&self) -> PathBuf {
        let _ = std::fs::remove_file(self.scratch.join("config.json"));
        self.scratch.write_file("config.json", serde_json::to_vec_pretty(&self.config).expect("json").as_slice(), 0o600)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.scratch.join("data")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.run.path().to_path_buf()
    }

    pub fn addr(&self) -> SocketAddr {
        ([127, 0, 0, 1], self.port).into()
    }

    /// Every file and directory below the data directory and the runtime directory, relative, sorted.
    pub fn tree(&self) -> Vec<String> {
        let mut found = Vec::new();
        for root in [self.data_dir(), self.run_dir()] {
            let mut stack = vec![root.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else { continue };
                for entry in entries.flatten() {
                    let path = entry.path();
                    found.push(path.strip_prefix(self.scratch.path()).unwrap_or(&path).display().to_string());
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(path);
                    }
                }
            }
        }
        found.sort();
        found
    }

    fn command(&self, with_key: bool) -> (Command, Vec<String>, Vec<(String, String)>) {
        let mut command = Command::new(BINARY);
        command.arg("serve").env_clear().stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        let mut env = vec![(ENV_CONFIG.to_owned(), self.config_path().display().to_string())];
        if with_key {
            env.push((ENV_KEY_FILE.to_owned(), self.key.display().to_string()));
        }
        for (name, value) in &env {
            command.env(name, value);
        }
        (command, vec!["serve".to_owned()], env)
    }

    /// Starts `serve` and waits until it is healthy. Panics with the captured standard error when it is not.
    pub fn start(self) -> Server {
        let (command, args, env) = self.command(true);
        let mut server = Server::spawn(self, command, args, env);
        server.wait_healthy();
        server
    }

    /// Starts `serve` expecting a refusal: answers how it ended. A process that is still running after `within` is killed
    /// and reported with no exit code.
    pub fn refuse(self, with_key: bool, within: Duration) -> Refusal {
        let before = self.tree();
        let (command, args, env) = self.command(with_key);
        let mut server = Server::spawn(self, command, args, env);
        let started = Instant::now();
        let code = loop {
            if let Some(status) = server.child.as_mut().expect("child").try_wait().expect("wait") {
                break status.code();
            }
            if started.elapsed() > within {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let elapsed = started.elapsed();
        let still_running = code.is_none();
        server.kill();
        std::thread::sleep(Duration::from_millis(100));
        let stderr = server.stderr_lines();
        let stdout = server.stdout_lines();
        let listening = std::net::TcpStream::connect_timeout(&server.site.addr(), Duration::from_millis(500)).is_ok();
        let after = server.site.tree();
        let new_files: Vec<String> = after.into_iter().filter(|f| !before.contains(f)).collect();
        Refusal { code, elapsed, stderr, stdout, listening, new_files, still_running }
    }
}

/// How a start that should have been refused ended.
#[derive(Debug)]
pub struct Refusal {
    pub code: Option<i32>,
    pub elapsed: Duration,
    pub stderr: Vec<String>,
    pub stdout: Vec<String>,
    pub listening: bool,
    pub new_files: Vec<String>,
    pub still_running: bool,
}

// ---- the process -------------------------------------------------------------------------------------------------------------

/// The server as a process, with its output captured.
pub struct Server {
    pub site: Site,
    child: Option<Child>,
    stdout: Arc<Mutex<Vec<String>>>,
    stderr: Arc<Mutex<Vec<String>>>,
    /// The argument list and the environment the process was started with (the same facts `/proc` shows on Linux).
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub started: Instant,
    pid: u32,
    pub recorder: Arc<Recorder>,
}

fn capture(stream: impl std::io::Read + Send + 'static, into: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            into.lock().expect("lock").push(line);
        }
    });
}

impl Server {
    fn spawn(site: Site, mut command: Command, args: Vec<String>, env: Vec<(String, String)>) -> Server {
        let mut child = command.spawn().expect("the server starts");
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        capture(child.stdout.take().expect("stdout"), stdout.clone());
        capture(child.stderr.take().expect("stderr"), stderr.clone());
        let pid = child.id();
        Server { site, child: Some(child), stdout, stderr, args, env, started: Instant::now(), pid, recorder: Arc::new(Recorder::default()) }
    }

    fn wait_healthy(&mut self) {
        let deadline = Instant::now() + START_WITHIN;
        loop {
            if let Some(status) = self.child.as_mut().expect("child").try_wait().expect("wait") {
                panic!("the server ended early with {status}: {}", self.stderr_lines().join(" | "));
            }
            if health::probe(&self.site.run_dir(), Duration::from_secs(3)) == Health::Healthy {
                return;
            }
            assert!(Instant::now() < deadline, "the server was not healthy within {START_WITHIN:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn stdout_lines(&self) -> Vec<String> {
        self.stdout.lock().expect("lock").clone()
    }

    pub fn stderr_lines(&self) -> Vec<String> {
        self.stderr.lock().expect("lock").clone()
    }

    pub fn is_running(&mut self) -> bool {
        self.child.as_mut().is_some_and(|c| c.try_wait().expect("wait").is_none())
    }

    /// The binary's own `healthcheck`, run against the same configuration.
    pub fn healthcheck(&self) -> (Option<i32>, String) {
        let output = Command::new(BINARY)
            .arg("healthcheck")
            .env_clear()
            .env(ENV_CONFIG, self.site.config_path())
            .output()
            .expect("healthcheck runs");
        (output.status.code(), String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// The one setup token line of standard output: exactly one line, starting with the text the product prints, else the test
    /// fails. Answers the token.
    pub fn setup_token(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lines = self.stdout_lines();
            if let Some(line) = lines.first() {
                assert_eq!(lines.len(), 1, "exactly one line on standard output");
                let word = line.split_whitespace().find(|word| word.starts_with("hwb_")).expect("the line holds a setup token");
                return word.trim_end_matches('.').to_owned();
            }
            assert!(Instant::now() < deadline, "no setup token line on standard output");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A client for this server.
    pub fn client(&self) -> Client {
        Client::new(self.site.addr(), self.site.der.clone(), self.recorder.clone())
    }

    /// Where the workers keep the homes of the people.
    pub fn homes(&self) -> PathBuf {
        self.site.data_dir().join("homes")
    }

    /// The home of the account with this uid.
    pub fn home_of(&self, uid: u32) -> PathBuf {
        self.homes().join(uid.to_string())
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.site.data_dir().join("audit")
    }

    /// Every line of the audit files, parsed.
    pub fn audit(&self) -> Vec<Value> {
        let mut files: Vec<PathBuf> =
            std::fs::read_dir(self.audit_dir()).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        files.sort();
        let mut rows = Vec::new();
        for file in files {
            for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
                rows.push(serde_json::from_str(line).unwrap_or_else(|e| panic!("an audit line is JSON ({e})")));
            }
        }
        rows
    }

    /// The process's environment and argument list as the system reports them where it can (Linux: `/proc`), else as the
    /// test spawned it.
    pub fn environment_and_arguments(&self) -> (Vec<u8>, Vec<u8>) {
        let pid = self.pid();
        let proc_env = std::fs::read(format!("/proc/{pid}/environ"));
        let proc_cmd = std::fs::read(format!("/proc/{pid}/cmdline"));
        match (proc_env, proc_cmd) {
            (Ok(env), Ok(cmd)) => (env, cmd),
            _ => {
                let env = self.env.iter().map(|(n, v)| format!("{n}={v}\0")).collect::<String>().into_bytes();
                let cmd = std::iter::once(BINARY.to_owned()).chain(self.args.clone()).map(|a| format!("{a}\0")).collect::<String>().into_bytes();
                (env, cmd)
            }
        }
    }

    /// True when the environment came from the system.
    pub fn environment_is_from_proc(&self) -> bool {
        Path::new(&format!("/proc/{}/environ", self.pid())).exists()
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// SIGTERM and wait for the exit: the status code, or the panic of the test when it does not stop in 20 seconds.
    pub fn stop(&mut self) -> Option<i32> {
        let mut child = self.child.take().expect("the server runs");
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).expect("pid")).expect("a pid");
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("signal");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = child.try_wait().expect("wait") {
                return status.code();
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the server did not stop within 20 seconds of SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

// ---- what the client saw -----------------------------------------------------------------------------------------------------

/// One answer the client received.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub method: String,
    pub target: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Everything the clients and sockets of one server received.
#[derive(Default)]
pub struct Recorder {
    exchanges: Mutex<Vec<Exchange>>,
    frames: Mutex<Vec<String>>,
}

impl Recorder {
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().expect("lock").clone()
    }

    pub fn frames(&self) -> Vec<String> {
        self.frames.lock().expect("lock").clone()
    }

    pub fn note(&self, method: &str, target: &str, reply: &Reply) {
        self.exchanges.lock().expect("lock").push(Exchange {
            method: method.to_owned(),
            target: target.to_owned(),
            status: reply.status,
            headers: reply.headers.clone(),
            body: reply.body.clone(),
        });
    }

    pub fn note_frame(&self, text: &str) {
        self.frames.lock().expect("lock").push(text.to_owned());
    }
}

// ---- the client --------------------------------------------------------------------------------------------------------------

/// A browser: a cookie jar, the anti-forgery token once a session exists, and the right `Origin`.
pub struct Client {
    addr: SocketAddr,
    der: CertificateDer<'static>,
    pub recorder: Arc<Recorder>,
    jar: BTreeMap<String, String>,
    pub csrf: Option<String>,
}

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

impl Client {
    pub fn new(addr: SocketAddr, der: CertificateDer<'static>, recorder: Arc<Recorder>) -> Client {
        Client { addr, der, recorder, jar: BTreeMap::new(), csrf: None }
    }

    /// The value of a cookie the jar holds.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.jar.get(name).map(String::as_str)
    }

    pub fn session_cookie(&self) -> Option<String> {
        self.cookie("__Host-hw_session").map(str::to_owned)
    }

    /// The request a page sends: `Origin` on a change, the token once a session exists, JSON when there is a body.
    pub fn page_request(&self, method: &str, target: &str, body: Option<&Value>) -> Request {
        let mut request = Request::new(method, target).header("User-Agent", USER_AGENT).close();
        if method != "GET" && method != "HEAD" {
            request = request.header("Origin", ORIGIN);
            if let Some(csrf) = &self.csrf {
                request = request.header("X-CSRF-Token", csrf);
            }
        }
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json");
            request.body = serde_json::to_vec(body).expect("json");
        }
        request
    }

    fn cookie_header(&self) -> Option<String> {
        if self.jar.is_empty() {
            return None;
        }
        Some(self.jar.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; "))
    }

    /// Sends `request` exactly as given plus the jar's cookies (unless it names a `Cookie` header itself), records the answer and
    /// keeps the cookies it sets.
    pub async fn send(&mut self, mut request: Request) -> Reply {
        if !request.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("cookie")) {
            if let Some(cookie) = self.cookie_header() {
                request = request.header("Cookie", &cookie);
            }
        }
        let reply = self.send_without_jar(&request).await;
        for line in reply.all("set-cookie") {
            let first = line.split(';').next().unwrap_or_default();
            let Some((name, value)) = first.split_once('=') else { continue };
            let deleted = line.to_ascii_lowercase().contains("max-age=0");
            if deleted || value.is_empty() {
                self.jar.remove(name);
            } else {
                self.jar.insert(name.to_owned(), value.to_owned());
            }
        }
        reply
    }

    /// Sends `request` exactly as given, touching neither the jar nor the headers; the answer is recorded.
    pub async fn send_without_jar(&self, request: &Request) -> Reply {
        let mut conn = Conn::new(tls_to(self.addr, &self.der).await);
        conn.send(&request.bytes()).await;
        let reply = conn.reply(request.method == "HEAD").await.expect("an answer");
        self.recorder.note(&request.method, &request.target, &reply);
        reply
    }

    /// As [`Client::send_without_jar`], for a request the server may refuse to answer at all: `None` when the connection ends
    /// without an answer. The jar's cookies are added, the answer is recorded.
    pub async fn try_send(&mut self, request: Request) -> Option<Reply> {
        let mut conn = Conn::new(tls_to(self.addr, &self.der).await);
        conn.send(&request.bytes()).await;
        let reply = conn.reply(request.method == "HEAD").await?;
        self.recorder.note(&request.method, &request.target.chars().take(80).collect::<String>(), &reply);
        Some(reply)
    }

    /// A call the page would make.
    pub async fn call(&mut self, method: &str, target: &str, body: Option<&Value>) -> Reply {
        let request = self.page_request(method, target, body);
        self.send(request).await
    }

    /// `GET /api/session`; keeps the anti-forgery token when the answer holds one.
    pub async fn session(&mut self) -> Value {
        let reply = self.call("GET", "/api/session", None).await;
        assert_eq!(reply.status, 200, "{}", reply.text());
        let value = reply.json();
        self.csrf = value.get("csrf").and_then(Value::as_str).map(str::to_owned);
        value
    }

    /// Enrolls through `route` (`/auth/bootstrap` or `/auth/invite/redeem`) with a password and the authenticator code computed
    /// from the secret in the answer. The client holds the new session afterwards.
    pub async fn enrol(&mut self, route: &str, token: &str, name: &str, password: &str) -> Enrolled {
        let begin = self
            .call(
                "POST",
                route,
                Some(&json!({"step": "begin", "token": token, "username": name, "display_name": name, "method": "password", "password": password})),
            )
            .await;
        assert_eq!(begin.status, 200, "begin: {}", begin.text());
        let secret = begin.json()["secret"].as_str().expect("an authenticator secret").to_owned();
        let secret_bytes = totp_rs::Secret::try_from_base32(secret.clone()).expect("base32").as_bytes().to_vec();
        let code = totp_code(&secret_bytes, unix_now());
        let finish = self.call("POST", route, Some(&json!({"step": "finish", "code": code}))).await;
        assert_eq!(finish.status, 200, "finish: {}", finish.text());
        let recovery = finish.json()["recovery_code"].as_str().expect("a recovery code").to_owned();
        self.session().await;
        assert!(self.csrf.is_some(), "the enrolment left a session");
        Enrolled { name: name.to_owned(), password: password.to_owned(), secret, recovery }
    }

    /// An administrator's invitation link token (`hwi_...`).
    pub async fn invitation(&mut self, role: &str) -> String {
        let reply = self.call("POST", "/api/admin/invitations", Some(&json!({"role": role, "days": 1}))).await;
        assert_eq!(reply.status, 200, "invitation: {}", reply.text());
        let link = reply.json()["link"].as_str().expect("a link").to_owned();
        link.split("#invite=").nth(1).expect("an invite fragment").to_owned()
    }

    /// Opens `/ws` with the jar's session cookie and the public origin.
    pub async fn socket(&self) -> Socket {
        let cookie = self.session_cookie().expect("a session");
        let request = crate::ws_env::WsEnv::upgrade_request(Some(&cookie), Some(ORIGIN), None);
        match WsClient::upgrade(tls_to(self.addr, &self.der).await, request).await {
            Ok(ws) => Socket { ws, recorder: self.recorder.clone(), queue: VecDeque::new(), next: 1 },
            Err(reply) => panic!("the upgrade was answered with {}: {}", reply.status, reply.text()),
        }
    }

    /// Tries the upgrade with a given request and answers the plain refusal, if any.
    pub async fn socket_with(&self, request: Request) -> Result<Socket, Reply> {
        WsClient::upgrade(tls_to(self.addr, &self.der).await, request)
            .await
            .map(|ws| Socket { ws, recorder: self.recorder.clone(), queue: VecDeque::new(), next: 1 })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// What the enrolment answers delivered to its owner.
#[derive(Debug, Clone)]
pub struct Enrolled {
    pub name: String,
    pub password: String,
    /// The authenticator secret, base32, as the one answer delivered it.
    pub secret: String,
    pub recovery: String,
}

/// Seconds since the epoch.
pub fn unix_now() -> i64 {
    i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("time").as_secs()).expect("time")
}

/// The six-digit authenticator code of RFC 6238 (HMAC-SHA1, 30-second step) at `unix`, computed with `ring`, independently of
/// the product's own implementation.
pub fn totp_code(secret: &[u8], unix: i64) -> String {
    let step = u64::try_from(unix / 30).expect("a positive time");
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = ring::hmac::sign(&key, &step.to_be_bytes());
    let digest = tag.as_ref();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let value = u32::from_be_bytes([digest[offset] & 0x7f, digest[offset + 1], digest[offset + 2], digest[offset + 3]]);
    format!("{:06}", value % 1_000_000)
}

// ---- the socket --------------------------------------------------------------------------------------------------------------

/// A WebSocket that records every text message it reads.
pub struct Socket {
    ws: WsClient,
    recorder: Arc<Recorder>,
    queue: VecDeque<Value>,
    next: u64,
}

impl Socket {
    async fn pump(&mut self, within: Duration) -> Option<Value> {
        loop {
            match self.ws.read(within).await {
                Got::Message(Incoming::Text(text)) => {
                    self.recorder.note_frame(&text);
                    return serde_json::from_str(&text).ok();
                }
                Got::Message(Incoming::Ping(_) | Incoming::Pong(_)) => {}
                Got::Message(Incoming::Close(code, reason)) => {
                    self.recorder.note_frame(&format!("close {code:?} {reason}"));
                    return None;
                }
                Got::Message(Incoming::Binary(bytes)) => {
                    self.recorder.note_frame(&String::from_utf8_lossy(&bytes));
                }
                Got::Quiet | Got::Ended => return None,
            }
        }
    }

    /// Sends one command and waits for its answer; other messages wait in the queue for `event_where`.
    pub async fn call(&mut self, cmd: &str, args: &Value) -> Value {
        let id = self.next;
        self.next += 1;
        let text = json!({"id": id, "cmd": cmd, "args": args}).to_string();
        self.ws.send_text(&text).await;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.pump(left).await {
                Some(value) if value.get("id").and_then(Value::as_u64) == Some(id) => return value,
                Some(other) => self.queue.push_back(other),
                None => panic!("no answer to {cmd}"),
            }
        }
    }

    /// The result of a command that must succeed.
    pub async fn ok(&mut self, cmd: &str, args: &Value) -> Value {
        let answer = self.call(cmd, args).await;
        assert!(answer.get("ok").is_some(), "{cmd} failed with {}", answer.get("err").map_or_else(String::new, |e| e["text"].as_str().map_or_else(|| e.to_string(), str::to_owned)));
        answer["ok"].clone()
    }

    /// The first event named `name` whose payload satisfies `wanted`, from the queue or the wire, within `within`.
    pub async fn event_where(&mut self, name: &str, within: Duration, wanted: impl Fn(&Value) -> bool) -> Option<Value> {
        if let Some(at) = self.queue.iter().position(|v| v.get("event").and_then(Value::as_str) == Some(name) && wanted(&v["payload"])) {
            return self.queue.remove(at);
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            let value = self.pump(left).await?;
            if value.get("event").and_then(Value::as_str) == Some(name) && wanted(&value["payload"]) {
                return Some(value);
            }
            self.queue.push_back(value);
        }
    }

    pub async fn close_code(&mut self, within: Duration) -> Option<(u16, String)> {
        self.ws.close_code(within).await
    }

    /// The pane `get_layout` focuses, after making sure it runs a shell.
    pub async fn start_focused_pane(&mut self) -> u64 {
        let view = self.ok("get_layout", &json!({})).await;
        let pane = view["focus"].as_u64().expect("a focused pane");
        self.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16})).await;
        pane
    }

    /// Types a line and presses Enter.
    pub async fn type_line(&mut self, pane: u64, line: &str) {
        self.ok("text", &json!({"pane": pane, "text": format!("{line}\r")})).await;
    }
}
