//! The server of the browser tests.
//!
//! This is not a test of the server. It is an ignored test that assembles the product's own server (`assemble`, the embedded
//! page, a TLS listener with a generated certificate for `hw.localhost`, `localhost` and `127.0.0.1`, workers in the process of the server, one
//! scratch directory) and keeps it running for a browser until its standard input closes. `app/tests/web/harness.mjs` starts
//! it; the page tests in `app/tests/web` drive a real browser against it.
//!
//! Protocol: every line this test prints for the runner starts with `HARNESS ` and holds one JSON object. It reads one JSON
//! object per line from standard input:
//!
//! - `{"op":"start", ...}` once, first. Options: `upload_max_bytes`. The answer is the event `ready` (origin, port, the one-time
//!   setup token, the certificate) after the health socket says that the server is ready.
//! - `{"op":"account","id":N, ...}` makes a person through the enrolment routes of the running server, exactly as a browser
//!   would, and answers `{"reply":N, ...}`. Options: `role` (`member` or `admin`), `name`, `passphrase` (true: a separate
//!   passphrase is set), `locked` (true: the password no longer opens the secret store), `state` (`disabled`). The answer holds
//!   the user name, the password, the authenticator secret, the recovery code and, if asked for, the passphrase. It is the only
//!   place where a secret is printed.
//! - `{"op":"stop"}` or the end of the input: the server stops, the scratch directory is removed, the test ends.
//!
//! Workers run in the server's own process here (`in-process-workers`): the isolation of the image is the subject of the tests
//! of the isolation and the image, not of these.
#![cfg(feature = "in-process-workers")]
#![allow(dead_code)]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::io::{BufRead, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::DirBuilderExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use edge_tls::{Conn, Reply, Request};
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock, SystemClock};
use holzwave_server::config::{self, Env, Prepared, RealProbe};
use holzwave_server::http::health::{self, Health};
use holzwave_server::http::routes_me::SecretsAdmin;
use holzwave_server::keyfile;
use holzwave_server::store::{AccountId, AccountState, Role, UnlockKind};
use holzwave_server::supervisor::serve::{Overrides, Running, assemble};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use ws_env::scratch::Scratch;

/// The host name of the server. Passkeys need a host name with a dot (the server refuses `localhost` for them), and a name below
/// `.localhost` reaches the loopback address in both engines without any entry in a hosts file.
const HOST: &str = "hw.localhost";

/// The longest the harness waits for the server it started.
const PATIENCE: Duration = Duration::from_secs(20);

/// The first line of the output: what this is and what it is not.
const FIRST_LINE: &str = "browser test harness: workers run in the server's own process (in-process-workers), not in the \
    container's isolated helper; the isolation is the subject of the isolation and image tests";

/// Prints one object for the runner.
fn say(value: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "HARNESS {value}");
    let _ = out.flush();
}

/// A writer the harness can read afterwards: the server prints the one-time setup token into it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    /// The token the server printed (`hwb_` and the rest), if it printed one.
    fn token(&self) -> Option<String> {
        let text = String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned();
        text.split_whitespace().find(|word| word.starts_with("hwb_")).map(str::to_owned)
    }
}

/// A free loopback port. The listener is closed again, so another process could take it before the server does; the caller
/// tries again when the start fails.
fn free_port() -> u16 {
    let probe = TcpListener::bind("127.0.0.1:0").expect("a free port");
    probe.local_addr().expect("an address").port()
}

/// A random lower-case hexadecimal text of `bytes` bytes.
fn random_hex(bytes: usize) -> String {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).expect("random bytes");
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

/// The running server and everything around it.
struct Harness {
    scratch: Scratch,
    run: Scratch,
    running: Option<Running>,
    addr: SocketAddr,
    origin: String,
    host_header: String,
    der: CertificateDer<'static>,
    tokens: Capture,
}

impl Harness {
    fn start(options: &Value) -> Result<Harness, String> {
        let mut last = String::from("no attempt");
        for _ in 0..5 {
            match Harness::try_start(options, free_port()) {
                Ok(harness) => return Ok(harness),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn try_start(options: &Value, port: u16) -> Result<Harness, String> {
        let scratch = Scratch::new("web");
        let run = Scratch::short("w");
        let key = scratch.join("server.key");
        keyfile::generate(&key).map_err(|e| format!("the key file: {e}"))?;
        let made = rcgen::generate_simple_self_signed(vec![HOST.to_owned(), "localhost".to_owned(), "127.0.0.1".to_owned()])
            .map_err(|e| format!("the certificate: {e}"))?;
        let cert = scratch.write_file("cert.pem", made.cert.pem().as_bytes(), 0o644);
        let cert_key = scratch.write_file("cert-key.pem", made.signing_key.serialize_pem().as_bytes(), 0o600);
        let data = scratch.join("data");
        std::fs::DirBuilder::new().mode(0o700).create(&data).map_err(|e| format!("the data directory: {e}"))?;
        let origin = format!("https://{HOST}:{port}");
        let mut config = json!({
            "public_url": origin,
            "listen": format!("127.0.0.1:{port}"),
            "data_dir": data,
            "runtime_dir": run.path(),
            "tls": {"mode": "files", "cert_file": cert, "key_file": cert_key},
            "auth": {"passkey": true, "password_totp": true},
            "workers": {"mode": "in_process", "uid_min": 20000, "uid_max": 29999}
        });
        if let Some(bytes) = options.get("upload_max_bytes").and_then(Value::as_u64) {
            config["upload"] = json!({"max_bytes": bytes});
        }
        let config_path = scratch.write_file("config.json", serde_json::to_vec_pretty(&config).expect("json").as_slice(), 0o600);
        let mut env = Env::new();
        env.insert(config::ENV_KEY_FILE.to_owned(), key.display().to_string());
        env.insert(config::ENV_CONFIG.to_owned(), config_path.display().to_string());
        let prepared: Prepared = config::prepare(&env, &RealProbe).map_err(|e| format!("the configuration is refused: {e}"))?;

        let tokens = Capture::default();
        let overrides = Overrides { token_writer: Some(Box::new(tokens.clone())), ..Overrides::default() };
        let running = tokio::runtime::Handle::current()
            .block_on(assemble(prepared, overrides))
            .map_err(|e| format!("the server does not start: {e}"))?;
        let addr = running.addr;
        Ok(Harness {
            scratch,
            run,
            running: Some(running),
            addr,
            origin,
            host_header: format!("{HOST}:{port}"),
            der: made.cert.der().clone(),
            tokens,
        })
    }

    fn running(&self) -> &Running {
        self.running.as_ref().expect("the server is running")
    }

    async fn wait_ready(&self) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if health::probe(self.run.path(), Duration::from_secs(3)) == Health::Healthy {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("the server did not become healthy".to_owned());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // ---- a client of the running server --------------------------------------------------------------------------------

    async fn connect(&self) -> TlsStream<TcpStream> {
        let mut store = RootCertStore::empty();
        store.add(self.der.clone()).expect("trust the generated certificate");
        let mut config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("client protocol versions")
            .with_root_certificates(store)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tcp = TcpStream::connect(self.addr).await.expect("connect");
        let name = ServerName::try_from(HOST).expect("server name");
        TlsConnector::from(Arc::new(config)).connect(name, tcp).await.expect("the TLS handshake")
    }

    /// One request, as a page would send it: the public `Host` and `Origin`.
    async fn send(&self, mut request: Request) -> Reply {
        request.headers[0].1 = self.host_header.clone();
        let request = request
            .header("Origin", &self.origin)
            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
            .close();
        let mut conn = Conn::new(self.connect().await);
        conn.send(&request.bytes()).await;
        conn.reply(request.method == "HEAD").await.expect("an answer")
    }

    async fn post(&self, path: &str, body: &Value, cookie: Option<(&str, &str)>) -> Reply {
        let mut request = Request::new("POST", path).header("Content-Type", "application/json");
        request.body = serde_json::to_vec(body).expect("json");
        if let Some((name, value)) = cookie {
            request = request.header("Cookie", &format!("{name}={value}"));
        }
        self.send(request).await
    }

    fn cookie_of(reply: &Reply, name: &str) -> Option<String> {
        reply
            .all("set-cookie")
            .iter()
            .find_map(|c| c.strip_prefix(&format!("{name}=")).map(|rest| rest.split(';').next().unwrap_or_default().to_owned()))
            .filter(|value| !value.is_empty())
    }

    // ---- people ----------------------------------------------------------------------------------------------------------

    /// The first active administrator, if there is one.
    fn an_admin(&self) -> Option<AccountId> {
        let store = &self.running().services.store;
        let all = store.read(|tx| tx.account_list()).ok()?;
        all.into_iter().find(|a| a.role == Role::Admin && a.state == AccountState::Active).map(|a| a.id)
    }

    /// A token for a new person of `role`: the setup token while nobody exists, an invitation otherwise (the harness makes a
    /// first administrator for it when the role asked for is the first one anyway).
    fn token_for(&self, role: Role) -> Result<(&'static str, String), String> {
        let accounts = &self.running().services.accounts;
        if let Some(admin) = self.an_admin() {
            let issued = accounts.invite_create(admin, None, None, role).map_err(|e| format!("the invitation: {e:?}"))?;
            return Ok(("/auth/invite/redeem", issued.token.expose().to_owned()));
        }
        let issued = accounts.bootstrap_issue().map_err(|e| format!("the setup token: {e:?}"))?;
        Ok(("/auth/bootstrap", issued.token.expose().to_owned()))
    }

    /// The code of the step before the current one: the enrolment spends it, and the first sign-in of the person then has the
    /// current step and the next one still to use.
    async fn earlier_code(secret: &[u8]) -> String {
        loop {
            let now = SystemClock.unix();
            // Too close to the end of a step: the server could count the next one before the code arrives.
            if now.rem_euclid(totp::STEP_SECONDS) >= totp::STEP_SECONDS - 2 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            return totp::code_at(secret, now - totp::STEP_SECONDS).expect("a code").to_string();
        }
    }

    async fn make_person(&self, options: &Value) -> Result<EnrolPerson, String> {
        let role = match options.get("role").and_then(Value::as_str).unwrap_or("member") {
            "admin" => Role::Admin,
            "member" => Role::Member,
            other => return Err(format!("the role {other:?} is not known")),
        };
        // A member needs an administrator to invite them; the first person is always an administrator.
        if role == Role::Member && self.an_admin().is_none() {
            let root = self.enrol("root-admin", Role::Admin).await?;
            self.end_session(root.session_cookie.as_deref()).await?;
        }
        let name = options
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}-{}", if role == Role::Admin { "admin" } else { "person" }, random_hex(3)));
        let mut person = self.enrol(&name, role).await?;
        let cookie = person.session_cookie.take();

        let wants_passphrase = options.get("passphrase").and_then(Value::as_bool).unwrap_or(false);
        let wants_locked = options.get("locked").and_then(Value::as_bool).unwrap_or(false);
        let running = self.running();
        let mut passphrase = None;
        if wants_passphrase || wants_locked {
            let cookie = cookie.as_deref().ok_or("the enrolment gave no session")?;
            let now = running.services.clock.unix();
            let session = running.services.sessions.lookup(cookie, now).map_err(|e| format!("the session: {e:?}"))?;
            let csrf = running.services.app.csrf_token(&session);
            if wants_passphrase {
                let text = format!("pass-{}-{}", random_hex(6), random_hex(6));
                let mut request = Request::new("PUT", "/api/me/passphrase")
                    .header("Content-Type", "application/json")
                    .header("X-CSRF-Token", &csrf)
                    .header("Cookie", &format!("__Host-hw_session={cookie}"));
                request.body = serde_json::to_vec(&json!({"passphrase": text})).expect("json");
                let reply = self.send(request).await;
                if reply.status != 204 {
                    return Err(format!("the passphrase was refused with {}", reply.status));
                }
                passphrase = Some(text);
            }
            if wants_locked {
                running
                    .services
                    .gate
                    .remove_slot(person.id, UnlockKind::Password, None)
                    .await
                    .map_err(|e| format!("the password slot: {e:?}"))?;
            }
        }
        // The session of the enrolment goes: the person signs in through the page, and the store is locked meanwhile.
        self.end_session(cookie.as_deref()).await?;
        self.wait_locked(person.id).await;
        if options.get("state").and_then(Value::as_str) == Some("disabled") {
            let admin = self.an_admin().ok_or("no administrator to disable the person")?;
            running.services.accounts.disable(admin, None, person.id).map_err(|e| format!("the disable: {e:?}"))?;
        }
        person.passphrase = passphrase;
        Ok(person)
    }

    /// Signs out the session of an enrolment through the route a page uses.
    async fn end_session(&self, cookie: Option<&str>) -> Result<(), String> {
        let Some(cookie) = cookie else { return Ok(()) };
        let running = self.running();
        let now = running.services.clock.unix();
        let Ok(session) = running.services.sessions.lookup(cookie, now) else { return Ok(()) };
        let csrf = running.services.app.csrf_token(&session);
        let mut request = Request::new("POST", "/auth/logout")
            .header("Content-Type", "application/json")
            .header("X-CSRF-Token", &csrf)
            .header("Cookie", &format!("__Host-hw_session={cookie}"));
        request.body = b"{}".to_vec();
        let reply = self.send(request).await;
        if reply.status >= 300 {
            return Err(format!("the sign-out of the enrolment session gave {}", reply.status));
        }
        Ok(())
    }

    /// Waits until the secret store of the person is no longer open (the last session ended).
    async fn wait_locked(&self, id: AccountId) {
        use holzwave_server::http::app::{StoreState, StoreView};
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.running().services.gate.state(id) == StoreState::Open && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Enrols a person with a password and an authenticator through the route a browser would use.
    async fn enrol(&self, name: &str, role: Role) -> Result<EnrolPerson, String> {
        let (route, token) = self.token_for(role)?;
        let password = format!("pw-{}-{}", random_hex(6), random_hex(6));
        let begin = self
            .post(
                route,
                &json!({"step": "begin", "token": token, "username": name, "display_name": name, "method": "password", "password": password}),
                None,
            )
            .await;
        if begin.status != 200 {
            return Err(format!("the first enrolment step gave {}", begin.status));
        }
        let pending = Harness::cookie_of(&begin, "__Host-hw_pending").ok_or("no pending cookie")?;
        let secret_text = begin.json()["secret"].as_str().ok_or("no authenticator secret")?.to_owned();
        let secret = totp_rs::Secret::try_from_base32(&secret_text).map_err(|e| format!("the secret: {e:?}"))?.as_bytes().to_vec();
        let code = Harness::earlier_code(&secret).await;
        let finish = self.post(route, &json!({"step": "finish", "code": code}), Some(("__Host-hw_pending", &pending))).await;
        if finish.status != 200 {
            return Err(format!("the second enrolment step gave {}", finish.status));
        }
        let session_cookie = Harness::cookie_of(&finish, "__Host-hw_session");
        let recovery = finish.json()["recovery_code"].as_str().ok_or("no recovery code")?.to_owned();
        let account = self
            .running()
            .services
            .store
            .read(|tx| tx.account_by_username(name))
            .map_err(|e| format!("the account: {e:?}"))?
            .ok_or("the account is missing")?;
        Ok(EnrolPerson { id: account.id, name: name.to_owned(), password, secret: secret_text, recovery, passphrase: None, session_cookie })
    }

    async fn stop(mut self) {
        if let Some(running) = self.running.take() {
            running.trigger();
            let _ = tokio::time::timeout(Duration::from_secs(15), running.join()).await;
        }
        let _ = &self.scratch;
    }
}

/// A person with the cookie of the enrolment session, until the harness has finished with it.
struct EnrolPerson {
    id: AccountId,
    name: String,
    password: String,
    secret: String,
    recovery: String,
    passphrase: Option<String>,
    session_cookie: Option<String>,
}

impl EnrolPerson {
    fn reply(&self, id: &Value) -> Value {
        let mut out = json!({
            "reply": id, "ok": true, "username": self.name, "password": self.password,
            "secret": self.secret, "recovery": self.recovery,
        });
        if let Some(passphrase) = &self.passphrase {
            out["passphrase"] = json!(passphrase);
        }
        out
    }
}

/// The lines of standard input, one JSON object each; `None` when the input ends.
fn read_commands() -> tokio::sync::mpsc::UnboundedReceiver<Option<Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(value) => {
                    if tx.send(Some(value)).is_err() {
                        return;
                    }
                }
                Err(_) => say(json!({"event": "error", "text": "a command line is not JSON"})),
            }
        }
        let _ = tx.send(None);
    });
    rx
}

#[test]
#[ignore = "started by app/tests/web/harness.mjs; it serves until its standard input closes"]
fn serve() {
    say(json!({"event": "info", "text": FIRST_LINE}));
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("a runtime");
    let _guard = runtime.enter();
    let mut commands = read_commands();

    // The first line carries the options.
    let options = match runtime.block_on(commands.recv()) {
        Some(Some(first)) if first["op"] == "start" => first,
        _ => {
            say(json!({"event": "error", "text": "the first command must be start"}));
            return;
        }
    };
    let harness = match Harness::start(&options) {
        Ok(h) => h,
        Err(text) => {
            say(json!({"event": "error", "text": text}));
            panic!("the harness could not start");
        }
    };
    if let Err(text) = runtime.block_on(harness.wait_ready()) {
        say(json!({"event": "error", "text": text}));
        runtime.block_on(harness.stop());
        panic!("the harness could not start");
    }
    say(json!({
        "event": "ready",
        "origin": harness.origin,
        "port": harness.addr.port(),
        "bootstrap": harness.tokens.token(),
        "cert_der_b64": STANDARD.encode(harness.der.as_ref()),
    }));

    runtime.block_on(async {
        while let Some(Some(command)) = commands.recv().await {
            let id = command.get("id").cloned().unwrap_or(Value::Null);
            match command["op"].as_str().unwrap_or_default() {
                "account" => match harness.make_person(&command).await {
                    Ok(person) => say(person.reply(&id)),
                    Err(text) => say(json!({"reply": id, "ok": false, "error": text})),
                },
                "stop" => break,
                other => say(json!({"reply": id, "ok": false, "error": format!("unknown command {other:?}")})),
            }
        }
        harness.stop().await;
    });
    say(json!({"event": "stopped"}));
}

