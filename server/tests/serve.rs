//! `serve`: the assembly of the whole server, the supervisor side of the secret store, the ordered shutdown, the token of the
//! first administrator and the real binary.
//!
//! The library tests assemble a server from a checked configuration in a scratch directory (a generated certificate, a loopback
//! port, workers in the server's own process) and talk to it over TLS. The process tests start the real binary. Everything here
//! needs the `in-process-workers` feature, and the page must have been built (`npm --prefix app run build`).
#![cfg(feature = "in-process-workers")]
#![allow(dead_code)]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/oidc_provider.rs"]
mod oidc_provider;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edge::{ORIGIN, TestApp, UID_RANGE};
use edge_tls::{Conn, Reply, Request};
use holzwave_server::auth::password::{ArgonGate, params_text};
use holzwave_server::auth::session::Method as SignInMethod;
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::{self, Env, Prepared, RealProbe, Refusal};
use holzwave_server::http::health::{self, Health};
use holzwave_server::http::routes_me::{SecretsAdmin, SecretsError};
use holzwave_server::http::routes_unlock::{GateError, ProvisionKey, UnlockGate, UnlockSecret};
use holzwave_server::http::app::{StoreState, StoreView};
use holzwave_server::keyfile;
use holzwave_server::store::{AccountId, NewAccount, Role, UnlockKind};
use holzwave_server::supervisor::gate::{GateServices, SupervisorGate};
use holzwave_server::supervisor::serve::{OidcClient, Overrides, Running, assemble, run_overrides};
use holzwave_server::supervisor::{AccountKey, InProcessLauncher, Supervisor, SupervisorSettings};
use holzwave_server::unlock::{Kdf, Kek, SALT_BYTES, generate_recovery_code, kek_from_passphrase, kek_from_recovery};
use holzwave_server::wire::{Ctl, CtlOutcome, Key, StoreState as WireState};
use holzwave_server::worker::secretstore::SlotKind;
use oidc_provider::{CLIENT_ID, ISSUER, MemoryHttp, SECRET, TestProvider};
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};
use ws_env::{WsClient, WsEnv, WsOptions, focus, tls_to};

const PATIENCE: Duration = Duration::from_secs(20);

// ---- a site: a configuration on disk ------------------------------------------------------------------------------------

/// A scratch directory with the key file, the certificate and the configuration of one server.
struct Site {
    scratch: ws_env::scratch::Scratch,
    run: ws_env::scratch::Scratch,
    config: Value,
    key: PathBuf,
    der: CertificateDer<'static>,
}

impl Site {
    fn new() -> Site {
        let scratch = ws_env::scratch::Scratch::new("serve");
        let key = scratch.join("server.key");
        keyfile::generate(&key).expect("a key file");
        let made = rcgen::generate_simple_self_signed(vec![edge::HOST.to_owned()]).expect("a certificate");
        let cert = scratch.write_file("cert.pem", made.cert.pem().as_bytes(), 0o644);
        let cert_key = scratch.write_file("cert-key.pem", made.signing_key.serialize_pem().as_bytes(), 0o600);
        let data = scratch.join("data");
        std::fs::DirBuilder::new().mode(0o700).create(&data).expect("a private directory");
        let run = ws_env::scratch::Scratch::short("r");
        let run_path = run.path().to_path_buf();
        let config = json!({
            "public_url": ORIGIN,
            "listen": "127.0.0.1:0",
            "data_dir": data,
            "runtime_dir": run_path,
            "tls": {"mode": "files", "cert_file": cert, "key_file": cert_key},
            "auth": {"passkey": true, "password_totp": true},
            "workers": {"mode": "in_process", "uid_min": 20000, "uid_max": 29999}
        });
        Site { scratch, run, config, key, der: made.cert.der().clone() }
    }

    fn set(&mut self, path: &str, value: Value) {
        let mut at = &mut self.config;
        let parts: Vec<&str> = path.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            at = at.as_object_mut().expect("an object").entry(*part).or_insert_with(|| json!({}));
        }
        at.as_object_mut().expect("an object").insert(parts[parts.len() - 1].to_owned(), value);
    }

    fn unset(&mut self, path: &str) {
        let mut at = &mut self.config;
        let parts: Vec<&str> = path.split('.').collect();
        for part in &parts[..parts.len() - 1] {
            at = at.get_mut(*part).expect("a section");
        }
        at.as_object_mut().expect("an object").remove(parts[parts.len() - 1]);
    }

    fn config_path(&self) -> PathBuf {
        let _ = std::fs::remove_file(self.scratch.join("config.json"));
        self.scratch.write_file("config.json", serde_json::to_vec_pretty(&self.config).expect("json").as_slice(), 0o600)
    }

    fn env(&self) -> Env {
        let mut env = Env::new();
        env.insert(config::ENV_KEY_FILE.to_owned(), self.key.display().to_string());
        env.insert(config::ENV_CONFIG.to_owned(), self.config_path().display().to_string());
        env
    }

    fn prepared(&self) -> Result<Prepared, Refusal> {
        config::prepare(&self.env(), &RealProbe)
    }

    fn run_dir(&self) -> PathBuf {
        self.run.path().to_path_buf()
    }

    fn data_dir(&self) -> PathBuf {
        self.scratch.join("data")
    }

    async fn start(&self) -> Running {
        self.start_with(Overrides::default()).await
    }

    async fn start_with(&self, overrides: Overrides) -> Running {
        let prepared = self.prepared().unwrap_or_else(|e| panic!("the configuration is refused: {e}"));
        assemble(prepared, overrides).await.unwrap_or_else(|e| panic!("the server does not start: {e}"))
    }

    /// Adds one single sign-on provider whose client secret is in a file.
    fn with_oidc(&mut self) {
        let secret = self.scratch.write_file("oidc-secret", format!("{SECRET}\n").as_bytes(), 0o600);
        self.set(
            "auth.oidc",
            json!([{
                "id": "corp", "name": "Company", "issuer": ISSUER, "client_id": CLIENT_ID,
                "client_secret_file": secret, "allowed_domains": ["example.org"]
            }]),
        );
    }

    async fn get(&self, addr: SocketAddr, path: &str) -> Reply {
        self.send(addr, Request::get(path).close()).await
    }

    async fn send(&self, addr: SocketAddr, request: Request) -> Reply {
        let mut conn = Conn::new(tls_to(addr, &self.der).await);
        conn.send(&request.bytes()).await;
        conn.reply(request.method == "HEAD").await.expect("an answer")
    }

    async fn socket(&self, addr: SocketAddr, cookie: &str) -> WsClient {
        let request = WsEnv::upgrade_request(Some(cookie), Some(ORIGIN), None);
        match WsClient::upgrade(tls_to(addr, &self.der).await, request).await {
            Ok(client) => client,
            Err(reply) => panic!("the upgrade was answered with {}: {}", reply.status, reply.text()),
        }
    }
}

/// A person with a session, made through the services of a running server.
fn person(running: &Running, name: &str) -> (AccountId, String) {
    let now = running.services.clock.unix();
    let account = running
        .services
        .store
        .write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), UID_RANGE, now))
        .expect("an account");
    let created = running
        .services
        .sessions
        .create(account.id, SignInMethod::PasswordTotp, now, "Firefox on Linux", Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))))
        .expect("a session");
    (account.id, created.value.expose().to_owned())
}

async fn stop(running: Running) {
    running.trigger();
    running.join().await.expect("the shutdown is done");
}

/// A writer a test can read afterwards.
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
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }
}

// ---- the assembly -------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn assemble_serves_the_page_over_tls_and_answers_the_health_check() {
    let site = Site::new();
    let running = site.start().await;
    let page = site.get(running.addr, "/").await;
    assert_eq!(page.status, 200);
    assert!(page.text().to_ascii_lowercase().contains("<!doctype html"), "the sign-in page is served");
    assert!(page.header("content-security-policy").is_some(), "the headers of the application layer are on");
    assert_eq!(health::probe(&site.run_dir(), Duration::from_secs(3)), Health::Healthy);
    assert!(running.readiness.ready());
    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assemble_lets_a_person_open_the_socket_and_call_a_command() {
    let site = Site::new();
    let running = site.start().await;
    let (_, cookie) = person(&running, "alice");
    let mut socket = site.socket(running.addr, &cookie).await;
    let first = socket.read_json(PATIENCE).await.expect("the session message");
    assert_eq!(first["event"], "hw-session");
    let view = socket.ok("get_layout", "{}").await;
    assert!(view.get("focus").is_some(), "{view}");
    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn assemble_refuses_compression_with_the_sentence_of_the_transport() {
    let mut site = Site::new();
    site.set("transport.compression", json!("deflate"));
    let prepared = site.prepared().expect("the file is valid");
    let refusal = assemble(prepared, Overrides::default()).await.err().expect("a refusal");
    assert!(refusal.sentence().contains("compression") && refusal.sentence().contains("deflate"), "{refusal}");
    assert!(!refusal.sentence().contains('\n'));
    assert!(!site.run_dir().join("health.sock").exists(), "nothing was left behind");
}

#[tokio::test(flavor = "multi_thread")]
async fn assemble_refuses_helper_mode_without_a_helper_in_one_sentence() {
    let mut site = Site::new();
    site.unset("workers.mode");
    let missing = site.scratch.join("no-such-helper");
    site.set("workers.helper_path", json!(missing));
    let sentence = match site.prepared() {
        Err(refusal) => refusal.sentence().to_owned(),
        Ok(prepared) => assemble(prepared, Overrides::default()).await.err().expect("a refusal").sentence().to_owned(),
    };
    assert!(sentence.contains("helper"), "{sentence}");
    assert!(!sentence.contains('\n'));
    assert!(!site.run_dir().join("health.sock").exists());
}

#[test]
fn the_default_overrides_are_neutral_and_run_uses_exactly_the_default() {
    let neutral = Overrides::default();
    assert!(neutral.listen.is_none());
    assert!(neutral.token_writer.is_none());
    assert!(neutral.grace.is_none());
    assert!(neutral.clock.is_none());
    assert!(neutral.oidc_client.is_none());
    assert_eq!(format!("{:?}", run_overrides()), format!("{neutral:?}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_injected_client_serves_the_single_sign_on_start_without_any_network() {
    let mut site = Site::new();
    site.with_oidc();
    let clock = Arc::new(ManualClock::new(edge::NOW));
    let provider = TestProvider::standard(clock.clone());
    let dyn_clock: Arc<dyn Clock> = clock;
    let overrides = Overrides {
        clock: Some(dyn_clock),
        oidc_client: Some(OidcClient::new(MemoryHttp::new(std::slice::from_ref(&provider)))),
        ..Overrides::default()
    };
    let running = site.start_with(overrides).await;
    let start = site.get(running.addr, "/auth/oidc/corp/start").await;
    assert_eq!(start.status, 302, "{}", start.text());
    let location = start.header("location").expect("a location");
    assert!(location.starts_with(ISSUER), "{location}");
    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_injected_client_a_provider_at_a_plain_http_address_is_refused_at_the_start() {
    let mut site = Site::new();
    site.with_oidc();
    let mut prepared = site.prepared().expect("a valid file");
    prepared.auth.oidc[0].issuer = "http://idp.example.test".to_owned();
    let refusal = assemble(prepared, Overrides::default()).await.err().expect("a refusal");
    assert!(refusal.sentence().contains("https"), "{refusal}");
    assert!(refusal.sentence().contains("corp"), "{refusal}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_route_list_has_every_route_of_the_contract_and_nothing_else() {
    let mut site = Site::new();
    site.with_oidc();
    let clock = Arc::new(ManualClock::new(edge::NOW));
    let provider = TestProvider::standard(clock.clone());
    let dyn_clock: Arc<dyn Clock> = clock;
    let overrides = Overrides {
        clock: Some(dyn_clock),
        oidc_client: Some(OidcClient::new(MemoryHttp::new(std::slice::from_ref(&provider)))),
        ..Overrides::default()
    };
    let running = site.start_with(overrides).await;
    let routes: [(&str, &str); 19] = [
        ("GET", "/"),
        ("GET", "/login.html"),
        ("GET", "/login.js"),
        ("GET", "/login.css"),
        ("GET", "/app.css"),
        ("GET", "/api/session"),
        ("POST", "/auth/password"),
        ("POST", "/auth/totp"),
        ("POST", "/auth/passkey/begin"),
        ("POST", "/auth/passkey/finish"),
        ("GET", "/auth/oidc/corp/start"),
        ("GET", "/auth/oidc/callback"),
        ("POST", "/auth/bootstrap"),
        ("POST", "/auth/invite/redeem"),
        ("POST", "/auth/logout"),
        ("POST", "/auth/unlock"),
        ("GET", "/ws"),
        ("GET", "/api/me"),
        ("GET", "/api/admin/accounts"),
    ];
    for (method, path) in routes {
        let reply = site
            .send(running.addr, Request::new(method, path).header("Origin", ORIGIN).header("Content-Type", "application/json").close())
            .await;
        assert_ne!(reply.status, 404, "{method} {path} is a route of the contract: {}", reply.text());
    }
    for path in ["/debug", "/metrics", "/healthz", "/status", "/.git/config", "/api/nope", "//", "/a/../b", "/config.json"] {
        let reply = site.get(running.addr, path).await;
        assert_eq!(reply.status, 404, "{path}");
        assert!(!reply.text().contains(path.trim_matches('/')) || path.trim_matches('/').is_empty(), "no detail in the body of {path}");
    }
    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shutdown_closes_the_sockets_stops_the_workers_and_removes_the_health_socket() {
    let site = Site::new();
    let running = site.start().await;
    let (alice, bob) = (person(&running, "alice"), person(&running, "bob"));
    let mut first = site.socket(running.addr, &alice.1).await;
    let mut second = site.socket(running.addr, &bob.1).await;
    let view = first.ok("get_layout", "{}").await;
    first
        .ok("start_pane", &json!({"pane": focus(&view), "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string())
        .await;
    second.ok("get_layout", "{}").await;
    assert_eq!(running.services.supervisor.stats().workers, 2);
    let health_socket = running.health_path.clone();
    assert!(health_socket.exists());
    let admission = running.admission.clone();
    running.trigger();
    running.trigger();
    assert_eq!(first.close_code(Duration::from_secs(5)).await, Some((1001, "going away".to_owned())));
    assert_eq!(second.close_code(Duration::from_secs(5)).await, Some((1001, "going away".to_owned())));
    let services = running.services.clone();
    running.join().await.expect("the shutdown is done");
    assert_eq!(services.supervisor.stats().workers, 0, "every worker ended");
    assert!(!health_socket.exists(), "the health socket is gone");
    drop((first, second));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while admission.open_connections() != 0 {
        assert!(std::time::Instant::now() < deadline, "the permits did not return to zero");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_is_up_while_running_and_down_as_soon_as_the_shutdown_begins() {
    let site = Site::new();
    let running = site.start().await;
    assert!(running.readiness.ready());
    assert!(running.readiness.failing().is_empty());
    running.trigger();
    assert!(!running.readiness.ready());
    assert_eq!(running.readiness.failing(), ["running"]);
    running.join().await.expect("the shutdown is done");
}

// ---- the token of the first administrator -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_first_start_prints_the_token_once_and_the_second_start_prints_nothing() {
    let site = Site::new();
    let first = Capture::default();
    let running = site.start_with(Overrides { token_writer: Some(Box::new(first.clone())), ..Overrides::default() }).await;
    let text = first.text();
    assert_eq!(text.lines().count(), 1, "one line: {text}");
    let token: String = text.split_whitespace().find(|word| word.starts_with("hwb_")).expect("a token in the line").trim_end_matches('.').to_owned();
    assert!(running.services.accounts.peek_bootstrap(&token).is_ok(), "the token works");
    stop(running).await;

    let second = Capture::default();
    let running = site.start_with(Overrides { token_writer: Some(Box::new(second.clone())), ..Overrides::default() }).await;
    assert_eq!(second.text(), "", "the earlier token is still valid, so nothing is printed");
    assert!(running.services.accounts.peek_bootstrap(&token).is_ok(), "and it still works");
    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_token_is_printed_once_an_account_exists() {
    let site = Site::new();
    let first = Capture::default();
    let running = site.start_with(Overrides { token_writer: Some(Box::new(first.clone())), ..Overrides::default() }).await;
    person(&running, "alice");
    stop(running).await;
    let second = Capture::default();
    let running = site.start_with(Overrides { token_writer: Some(Box::new(second.clone())), ..Overrides::default() }).await;
    assert_eq!(second.text(), "");
    stop(running).await;
}

#[test]
fn the_assembly_logs_no_token_and_holds_no_unsafe_code() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/supervisor");
    for name in ["serve.rs", "gate.rs"] {
        let source = std::fs::read_to_string(root.join(name)).expect("source");
        let code: String = source.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        assert!(!code.contains("unsafe"), "{name}");
        for call in code.split("log::event(").skip(1) {
            let call = call.split(");").next().unwrap_or_default();
            // Only the code counts: the text between quotes names the event, it is not a value.
            let code: String = call.split('"').step_by(2).collect();
            assert!(!code.contains("token") && !code.contains("issued") && !code.contains("key"), "{name}: {call}");
        }
    }
}

// ---- the gate -----------------------------------------------------------------------------------------------------------

struct GateEnv {
    test: TestApp,
    scratch: ws_env::scratch::Scratch,
    launcher: Arc<InProcessLauncher>,
    supervisor: Supervisor,
    gate: Arc<SupervisorGate>,
    argon: ArgonGate,
    account: AccountId,
    key: AccountKey,
}

fn gate_env_with(argon: ArgonGate) -> GateEnv {
    let test = TestApp::new();
    let account = test.create_account("alice", Role::Member);
    let found = test.store.account_get(account).expect("read").expect("the account");
    let scratch = ws_env::scratch::Scratch::new("gate");
    let shell = holzwave_host::state::ShellSpec::new("/bin/sh", &[]);
    let launcher = Arc::new(InProcessLauncher::new(scratch.path().to_path_buf(), shell));
    let supervisor = Supervisor::new(launcher.clone(), SupervisorSettings::default(), test.clock.clone());
    let key = AccountKey { id: account, uid: found.uid };
    let gate = Arc::new(SupervisorGate::new(services(&test, &supervisor, &argon)));
    GateEnv { test, scratch, launcher, supervisor, gate, argon, account, key }
}

fn services(test: &TestApp, supervisor: &Supervisor, argon: &ArgonGate) -> GateServices {
    GateServices {
        supervisor: supervisor.clone(),
        store: test.store.clone(),
        sessions: test.sessions.clone(),
        audit: test.app.audit.clone(),
        clock: test.clock.clone(),
        argon: argon.clone(),
    }
}

fn services_for(env: &GateEnv, supervisor: &Supervisor, argon: &ArgonGate) -> GateServices {
    services(&env.test, supervisor, argon)
}

fn gate_env() -> GateEnv {
    gate_env_with(ArgonGate::new(2, Kdf::lower_bound()))
}

fn kek(byte: u8) -> Kek {
    Kek::from_bytes(&[byte; 32])
}

fn provision_key(kind: UnlockKind, byte: u8) -> ProvisionKey {
    ProvisionKey::new(kind, None, &kek(byte))
}

impl GateEnv {
    /// Locks the store of the worker (the key in its memory is wiped) and then opens it with the key as a new sign-in would:
    /// the worker reads the slots from the file again. This answers what a worker started from nothing would answer.
    async fn reopen_on(&self, supervisor: &Supervisor, kind: SlotKind, bytes: [u8; 32]) -> CtlOutcome {
        let _ = supervisor.ctl(self.key, Ctl::Lock { ctl: 0 }).await;
        supervisor.ctl(self.key, Ctl::Unlock { ctl: 0, kind, key: Key::from_bytes(&bytes) }).await.expect("an answer")
    }

    async fn reopen(&self, kind: SlotKind, bytes: [u8; 32]) -> CtlOutcome {
        self.reopen_on(&self.supervisor, kind, bytes).await
    }

    async fn lock(&self) {
        assert!(matches!(self.supervisor.ctl(self.key, Ctl::Lock { ctl: 0 }).await, Ok(CtlOutcome::Done)));
        self.wait_state(Some(WireState::Locked)).await;
    }

    async fn wait_state(&self, wanted: Option<WireState>) {
        let mut watch = self.gate.subscribe(self.account).expect("the account exists");
        tokio::time::timeout(PATIENCE, watch.wait_for(|state| *state == wanted))
            .await
            .unwrap_or_else(|_| panic!("the store did not reach {wanted:?}"))
            .expect("the channel is open");
    }

    fn audit_lines(&self) -> usize {
        let mut lines = 0;
        if let Ok(entries) = std::fs::read_dir(&self.test.audit_dir) {
            for entry in entries.flatten() {
                lines += std::fs::read_to_string(entry.path()).map(|t| t.lines().count()).unwrap_or(0);
            }
        }
        lines
    }

    /// Gives the person a passphrase slot and a stored passphrase method (with the cheapest parameters), then locks the store.
    async fn with_passphrase(&self, passphrase: &str) {
        let salt = [5u8; SALT_BYTES];
        let kdf = Kdf::lower_bound();
        let derived = kek_from_passphrase(passphrase, &salt, &kdf).expect("a key");
        self.test
            .store
            .write(|tx| tx.unlock_add(self.account, UnlockKind::Passphrase, &salt, &params_text(&kdf), None))
            .expect("the stored method");
        let keys = vec![ProvisionKey::new(UnlockKind::Passphrase, None, &derived)];
        self.gate.provision(self.account, keys).await.expect("provision");
        self.wait_state(Some(WireState::Unlocked)).await;
        assert!(matches!(self.supervisor.ctl(self.key, Ctl::Lock { ctl: 0 }).await, Ok(CtlOutcome::Done)));
        self.wait_state(Some(WireState::Locked)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_that_does_not_exist_or_is_not_known_counts_as_locked() {
    let env = gate_env();
    assert_eq!(env.gate.state(env.account), StoreState::Locked, "no worker, no store");
    assert_eq!(env.gate.state(AccountId::from_raw(987_654)), StoreState::Locked, "an account that does not exist");
    env.supervisor.ensure(env.key).await.expect("a worker");
    env.wait_state(Some(WireState::Absent)).await;
    assert_eq!(env.gate.state(env.account), StoreState::Locked, "a running worker with no store file");
    env.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provision_creates_the_store_that_either_key_opens_in_a_fresh_process_and_a_second_provision_is_refused() {
    let env = gate_env();
    let audit_before = env.audit_lines();
    let keys = vec![provision_key(UnlockKind::Password, 1), provision_key(UnlockKind::Recovery, 2)];
    env.gate.provision(env.account, keys).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    assert_eq!(env.gate.state(env.account), StoreState::Open);
    let again = env.gate.provision(env.account, vec![provision_key(UnlockKind::Password, 3)]).await;
    assert_eq!(again, Err(GateError::Refused), "a store exists already");
    assert_eq!(env.reopen(SlotKind::Password, [1; 32]).await, CtlOutcome::Done, "the first key opens a fresh worker");
    assert_eq!(env.reopen(SlotKind::Recovery, [2; 32]).await, CtlOutcome::Done, "so does the second");
    assert_eq!(env.reopen(SlotKind::Password, [9; 32]).await, CtlOutcome::Locked, "a key that is neither opens nothing");
    assert_eq!(env.audit_lines(), audit_before, "the gate writes no audit line");
}

#[tokio::test(flavor = "multi_thread")]
async fn set_slot_adds_a_slot_and_a_second_key_replaces_one_on_a_locked_store() {
    let env = gate_env();
    env.gate.provision(env.account, vec![provision_key(UnlockKind::Password, 1)]).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    // A passphrase slot on the open store.
    SecretsAdmin::set_slot(&*env.gate, env.account, provision_key(UnlockKind::Passphrase, 4), None).await.expect("a new slot");
    assert_eq!(env.reopen(SlotKind::Passphrase, [4; 32]).await, CtlOutcome::Done, "the new slot opens a fresh worker");
    // With the store locked, a password change opens it with the old key.
    env.lock().await;
    let replaced = SecretsAdmin::set_slot(
        &*env.gate,
        env.account,
        provision_key(UnlockKind::Password, 7),
        Some(provision_key(UnlockKind::Password, 1)),
    )
    .await;
    assert_eq!(replaced, Ok(()));
    assert_eq!(env.gate.state(env.account), StoreState::Locked, "the store is locked again afterwards");
    assert_eq!(env.reopen(SlotKind::Password, [7; 32]).await, CtlOutcome::Done, "the new password key opens it");
    assert_eq!(env.reopen(SlotKind::Password, [1; 32]).await, CtlOutcome::Locked, "the old one does not");
    // A key that opens nothing is refused with the locked answer.
    let wrong = SecretsAdmin::set_slot(
        &*env.gate,
        env.account,
        provision_key(UnlockKind::Password, 8),
        Some(provision_key(UnlockKind::Password, 99)),
    )
    .await;
    assert_eq!(wrong, Err(SecretsError::Locked));
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_slot_removes_one_slot_and_refuses_the_last() {
    let env = gate_env();
    let keys = vec![provision_key(UnlockKind::Password, 1), provision_key(UnlockKind::Recovery, 2)];
    env.gate.provision(env.account, keys).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    SecretsAdmin::remove_slot(&*env.gate, env.account, UnlockKind::Recovery, None).await.expect("remove one");
    assert_eq!(env.reopen(SlotKind::Recovery, [2; 32]).await, CtlOutcome::Locked, "the removed key opens nothing");
    assert_eq!(env.reopen(SlotKind::Password, [1; 32]).await, CtlOutcome::Done, "the other slot is untouched");
    let last = SecretsAdmin::remove_slot(&*env.gate, env.account, UnlockKind::Password, None).await;
    assert!(matches!(last, Err(SecretsError::Refused | SecretsError::Locked)), "the last slot stays: {last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_removes_the_store_with_the_worker_running_and_with_it_stopped() {
    let env = gate_env();
    env.gate.provision(env.account, vec![provision_key(UnlockKind::Password, 1)]).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    // With the worker running.
    SecretsAdmin::reset(&*env.gate, env.account).await.expect("reset");
    env.wait_state(Some(WireState::Absent)).await;
    assert_eq!(env.gate.state(env.account), StoreState::Locked);
    assert_eq!(env.reopen(SlotKind::Password, [1; 32]).await, CtlOutcome::Absent, "a fresh process finds no store");
    // With the worker stopped: a store again, then the wipe job.
    let env2 = gate_env();
    env2.gate.provision(env2.account, vec![provision_key(UnlockKind::Password, 1)]).await.expect("provision");
    env2.wait_state(Some(WireState::Unlocked)).await;
    env2.supervisor.shutdown().await;
    let after = Supervisor::new(env2.launcher.clone(), SupervisorSettings::default(), env2.test.clock.clone());
    let gate = SupervisorGate::new(services_for(&env2, &after, &env2.argon));
    SecretsAdmin::reset(&gate, env2.account).await.expect("reset with no worker");
    assert_eq!(after.stats().workers, 0, "the wipe job started no shell and left no worker");
    assert_eq!(env2.reopen_on(&after, SlotKind::Password, [1; 32]).await, CtlOutcome::Absent);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passphrase_unlocks_the_store_and_a_wrong_one_changes_nothing() {
    let env = gate_env();
    env.with_passphrase("a long passphrase for the store").await;
    let wrong = env.gate.unlock_with(env.account, UnlockSecret::Passphrase("not the passphrase")).await;
    assert_eq!(wrong, Err(GateError::Refused));
    assert_eq!(env.gate.state(env.account), StoreState::Locked, "a refusal changes nothing");
    env.gate.unlock_with(env.account, UnlockSecret::Passphrase("a long passphrase for the store")).await.expect("unlocked");
    env.wait_state(Some(WireState::Unlocked)).await;
    assert_eq!(env.gate.state(env.account), StoreState::Open);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovery_code_unlocks_the_store_and_a_wrong_one_changes_nothing() {
    let env = gate_env();
    let code = generate_recovery_code().expect("a code");
    let salt = [6u8; 16];
    let derived = kek_from_recovery(&code, &salt).expect("a key");
    let shown = code.display();
    env.test
        .store
        .write(|tx| tx.unlock_add(env.account, UnlockKind::Recovery, &salt, "hkdf-sha256", None))
        .expect("the stored method");
    env.gate.provision(env.account, vec![ProvisionKey::new(UnlockKind::Recovery, None, &derived)]).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    assert!(matches!(env.supervisor.ctl(env.key, Ctl::Lock { ctl: 0 }).await, Ok(CtlOutcome::Done)));
    env.wait_state(Some(WireState::Locked)).await;
    let wrong = env.gate.unlock_with(env.account, UnlockSecret::RecoveryCode("AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA")).await;
    assert_eq!(wrong, Err(GateError::Refused));
    let garbage = env.gate.unlock_with(env.account, UnlockSecret::RecoveryCode("not a code")).await;
    assert_eq!(garbage, Err(GateError::Refused));
    assert_eq!(env.gate.state(env.account), StoreState::Locked);
    env.gate.unlock_with(env.account, UnlockSecret::RecoveryCode(&shown)).await.expect("unlocked");
    env.wait_state(Some(WireState::Unlocked)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_unlocks_at_once_end_in_one_unlocked_state_and_no_error() {
    let env = gate_env();
    env.with_passphrase("a long passphrase for the store").await;
    let gate = env.gate.clone();
    let account = env.account;
    let one = tokio::spawn({
        let gate = gate.clone();
        async move { gate.unlock_with(account, UnlockSecret::Passphrase("a long passphrase for the store")).await }
    });
    let two = tokio::spawn({
        let gate = gate.clone();
        async move { gate.unlock_with(account, UnlockSecret::Passphrase("a long passphrase for the store")).await }
    });
    assert_eq!(one.await.expect("task"), Ok(()));
    assert_eq!(two.await.expect("task"), Ok(()));
    env.wait_state(Some(WireState::Unlocked)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_derivation_gate_is_an_unavailable_answer_and_not_a_panic() {
    let argon = ArgonGate::new(1, Kdf::lower_bound()).with_hook(Arc::new(|| std::thread::sleep(Duration::from_millis(150))));
    let env = gate_env_with(argon);
    env.with_passphrase("a long passphrase for the store").await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let gate = env.gate.clone();
        let account = env.account;
        tasks.spawn(async move { gate.unlock_with(account, UnlockSecret::Passphrase("a long passphrase for the store")).await });
    }
    let mut busy = false;
    while let Some(done) = tasks.join_next().await {
        if done.expect("no task panics") == Err(GateError::Unavailable) {
            busy = true;
            break;
        }
    }
    assert!(busy, "a flood of derivations is answered as unavailable");
    tasks.abort_all();
}

#[tokio::test(flavor = "multi_thread")]
async fn no_key_byte_reaches_a_debug_text_or_an_audit_line() {
    let env = gate_env();
    let audit_before = env.audit_lines();
    let canary = ProvisionKey::new(UnlockKind::Password, None, &Kek::from_bytes(&[0xC7; 32]));
    let texts = [
        format!("{:?}", env.gate),
        format!("{:?}", env.gate.services()),
        format!("{canary:?}"),
        format!("{:?}", Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key: Key::from_bytes(&[0xC7; 32]) }),
        format!("{:?}", Key::from_bytes(&[0xC7; 32])),
    ];
    env.gate.provision(env.account, vec![canary]).await.expect("provision");
    env.wait_state(Some(WireState::Unlocked)).await;
    for text in texts {
        assert!(!text.contains("199") && !text.to_ascii_lowercase().contains("c7c7"), "{text}");
    }
    assert_eq!(env.audit_lines(), audit_before, "no gate call wrote an audit line");
    env.supervisor.shutdown().await;
}

// ---- the tracer once more, with the real gate ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_whole_chain_runs_with_the_real_gate_and_the_list_of_the_assembly() {
    let env = WsEnv::start_with(WsOptions { real_gate: true, ..WsOptions::default() }).await;
    let alice = env.person("alice");
    let mut socket = env.connect_as(&alice.signed).await;
    let first = socket.read_json(PATIENCE).await.expect("the session message");
    assert_eq!(first["payload"]["state"], "ok");
    let locked = socket.event_where("hw-session", PATIENCE, |p| p["state"] == "locked").await;
    assert!(locked.is_some(), "a person with no store is told that the store is locked: the gate is the view");
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string()).await;
    socket.ok("text", &json!({"pane": pane, "text": "printf 'hw-%s\\n' e2e\r"}).to_string()).await;
    let seen = socket.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains("hw-e2e")).await;
    assert!(seen.is_some(), "the output of the shell comes back");
    // The gate opens the store of the person (a provision under a key), and the next look at the session says so.
    let gate = env.gate.as_ref().expect("the real gate");
    gate.provision(alice.key.id, vec![provision_key(UnlockKind::Password, 1)]).await.expect("provision");
    env.eventually("the store is open", |e| e.gate.as_ref().is_some_and(|g| g.state(alice.key.id) == StoreState::Open)).await;
}

// ---- the real binary ----------------------------------------------------------------------------------------------------

fn free_port() -> u16 {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
    socket.local_addr().expect("address").port()
}

/// The server started as a process, with the lines of its standard output arriving on a channel.
struct Process {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

fn spawn_server(site: &Site, with_key: bool) -> Process {
    let mut command = Command::new(env!("CARGO_BIN_EXE_holzwave-server"));
    command.arg("serve").env_clear().stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    command.env(config::ENV_CONFIG, site.config_path());
    if with_key {
        command.env(config::ENV_KEY_FILE, &site.key);
    }
    let mut child = command.spawn().expect("the server starts");
    let (sender, lines) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().expect("stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = stderr.clone();
    let err = child.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let mut all = sink.lock().expect("lock");
            all.push_str(&line);
            all.push('\n');
        }
    });
    Process { child, lines, stderr }
}

fn wait_healthy(site: &Site, process: &mut Process) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = process.child.try_wait().expect("wait") {
            panic!("the server ended early with {status}: {}", process.stderr.lock().expect("lock"));
        }
        if health::probe(&site.run_dir(), Duration::from_secs(3)) == Health::Healthy {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "the server did not become healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn process_serve_runs_redeems_the_token_opens_the_socket_and_stops_on_sigterm() {
    let mut site = Site::new();
    let port = free_port();
    site.set("listen", json!(format!("127.0.0.1:{port}")));
    let started = std::time::Instant::now();
    let mut process = spawn_server(&site, true);
    wait_healthy(&site, &mut process);
    println!("process: healthy after {:?}", started.elapsed());

    // `healthcheck` of the same binary agrees.
    let check = Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
        .arg("healthcheck")
        .env_clear()
        .env(config::ENV_CONFIG, site.config_path())
        .output()
        .expect("healthcheck runs");
    assert_eq!(check.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&check.stdout).trim(), "healthy");

    // Standard output holds the one line with the token, and standard error holds no token.
    let line = process.lines.recv_timeout(Duration::from_secs(10)).expect("the line with the token");
    assert!(process.lines.recv_timeout(Duration::from_millis(300)).is_err(), "exactly one line on standard output");
    let token: String = line.split_whitespace().find(|word| word.starts_with("hwb_")).expect("a token").trim_end_matches('.').to_owned();
    assert!(!process.stderr.lock().expect("lock").contains(&token), "the token is not in the log");

    // The token redeems through the enrolment flow, and the new session opens the socket.
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let post = |body: Value, cookie: Option<(&str, &str)>| {
        let mut request = Request::new("POST", "/auth/bootstrap")
            .header("Origin", ORIGIN)
            .header("Content-Type", "application/json")
            .close();
        request.body = serde_json::to_vec(&body).expect("json");
        if let Some((name, value)) = cookie {
            request = request.header("Cookie", &format!("{name}={value}"));
        }
        request
    };
    let begin = site
        .send(
            addr,
            post(json!({"step": "begin", "token": token, "username": "alice", "display_name": "Alice", "method": "password", "password": "correct horse battery staple"}), None),
        )
        .await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    let pending = begin
        .all("set-cookie")
        .iter()
        .find_map(|c| c.strip_prefix("__Host-hw_pending=").map(|rest| rest.split(';').next().unwrap_or_default().to_owned()))
        .expect("a pending cookie");
    let secret = begin.json()["secret"].as_str().expect("a secret").to_owned();
    let secret = totp_rs::Secret::try_from_base32(secret).expect("base32").as_bytes().to_vec();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("time").as_secs();
    let code = holzwave_server::auth::totp::code_at(&secret, i64::try_from(now).expect("time")).expect("a code").to_string();
    let finish = site.send(addr, post(json!({"step": "finish", "code": code}), Some(("__Host-hw_pending", &pending)))).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let cookie = finish
        .all("set-cookie")
        .iter()
        .find_map(|c| c.strip_prefix("__Host-hw_session=").map(|rest| rest.split(';').next().unwrap_or_default().to_owned()))
        .expect("a session cookie");
    let mut socket = site.socket(addr, &cookie).await;
    let view = socket.ok("get_layout", "{}").await;
    assert!(view.get("focus").is_some());

    // SIGTERM: the process exits 0 within the grace time and leaves no health socket.
    let pid = rustix::process::Pid::from_raw(i32::try_from(process.child.id()).expect("pid")).expect("a pid");
    let signalled = std::time::Instant::now();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("signal");
    assert_eq!(socket.close_code(Duration::from_secs(10)).await.map(|c| c.0), Some(1001));
    let status = loop {
        if let Some(status) = process.child.try_wait().expect("wait") {
            break status;
        }
        assert!(signalled.elapsed() < Duration::from_secs(20), "the server did not stop in time");
        std::thread::sleep(Duration::from_millis(50));
    };
    println!("process: stopped {:?} after SIGTERM with {status}", signalled.elapsed());
    assert_eq!(status.code(), Some(0));
    assert!(!site.run_dir().join("health.sock").exists());
    let all_stderr = process.stderr.lock().expect("lock").clone();
    assert!(!all_stderr.contains(&token) && !all_stderr.contains("correct horse"), "no secret in the log");
}

#[test]
fn process_serve_refuses_an_unsafe_start_with_one_sentence_and_no_listener() {
    // A missing key file.
    let mut site = Site::new();
    let port = free_port();
    site.set("listen", json!(format!("127.0.0.1:{port}")));
    // A missing public_url, plain HTTP without a proxy and a missing key file each exit 1.
    let mut cases: Vec<(&str, Site, bool)> = Vec::new();
    let mut no_key = Site::new();
    no_key.set("listen", json!(format!("127.0.0.1:{port}")));
    cases.push(("no key file", no_key, false));
    let mut no_url = Site::new();
    no_url.set("listen", json!(format!("127.0.0.1:{port}")));
    no_url.unset("public_url");
    cases.push(("no public_url", no_url, true));
    let mut plain = Site::new();
    plain.set("listen", json!(format!("127.0.0.1:{port}")));
    plain.set("public_url", json!("http://hw.example.test"));
    cases.push(("plain http", plain, true));
    let mut no_tls = Site::new();
    no_tls.set("listen", json!(format!("127.0.0.1:{port}")));
    no_tls.unset("tls");
    cases.push(("no tls", no_tls, true));
    let mut no_method = Site::new();
    no_method.set("listen", json!(format!("127.0.0.1:{port}")));
    no_method.set("auth", json!({"passkey": false, "password_totp": false}));
    cases.push(("no sign-in method", no_method, true));
    for (name, case, with_key) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
            .arg("serve")
            .env_clear()
            .env(config::ENV_CONFIG, case.config_path())
            .envs(with_key.then_some((config::ENV_KEY_FILE, case.key.clone())))
            .output()
            .expect("the server runs");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(output.status.code(), Some(1), "{name}: {stderr}");
        assert_eq!(stderr.trim_end().lines().count(), 1, "{name}: one sentence on standard error: {stderr}");
        assert!(output.stdout.is_empty(), "{name}: nothing on standard output");
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err(), "{name}: no listener");
    }
}

#[test]
fn the_usage_text_names_serve() {
    let output = Command::new(env!("CARGO_BIN_EXE_holzwave-server")).env_clear().output().expect("the server runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("serve"));
}
