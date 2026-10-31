//! The environment of the unlock tests: the WebSocket environment with the real gate, real in-process workers, real sessions
//! and a manual clock, plus the people, their sessions and stores made through the real routes, the sweep of the gate, the audit
//! reader and the scanner that looks for secrets in every byte the server leaves behind.
//!
//! Tests pull it in with `#[path]` after `edge.rs`, `edge_tls.rs` and `ws_env.rs`. Everything here needs the
//! `in-process-workers` feature.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use holzwave_server::accounts::Accounts;
use holzwave_server::clock::Clock;
use holzwave_server::http::app::{StoreState, StoreView};
use holzwave_server::store::{AccountId, Role};
use holzwave_server::supervisor::AccountKey;
use holzwave_server::supervisor::gate::SupervisorGate;
use holzwave_server::supervisor::registry::AttachError;
use holzwave_server::supervisor::socket::Outbound;
use holzwave_server::worker::secretstore::store_path;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::edge::{ORIGIN, Signed, UID_RANGE};
use crate::edge_tls::{Reply, Request};
use crate::ws_env::{PATIENCE, WsClient, WsEnv, WsOptions};

/// The seconds between two sign-ins of the same person: more than the 30 seconds of one authenticator code.
pub const CODE_STEP: i64 = 31;

/// A password that passes the policy.
pub const PASSWORD: &str = "correct horse battery";

/// A person made through the enrolment routes.
#[derive(Clone)]
pub struct User {
    pub name: String,
    pub password: String,
    /// The authenticator secret.
    pub secret: Vec<u8>,
    pub id: AccountId,
    pub key: AccountKey,
    /// The recovery code the enrolment answer showed.
    pub recovery: String,
}

/// One row of the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub action: String,
    pub result: String,
    pub actor: String,
    pub target: String,
    pub detail: Value,
}

impl Row {
    /// `action` and `result` as one word, `secrets.lock/ok`.
    pub fn word(&self) -> String {
        format!("{}/{}", self.action, self.result)
    }

    /// The text of a member of `detail`.
    pub fn detail_text(&self, key: &str) -> Option<String> {
        self.detail.get(key).and_then(Value::as_str).map(str::to_owned)
    }
}

/// A place in a file where a needle was found: never the needle itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub offset: usize,
    pub needle: String,
}

/// The environment.
pub struct UnlockEnv {
    pub ws: WsEnv,
    /// A second view of the same accounts, for the tokens of enrolment only.
    pub accounts: Accounts,
    pub gate: Arc<SupervisorGate>,
}

impl UnlockEnv {
    /// The default environment: the real gate, the real worker, 30 minutes idle and 24 hours absolute.
    pub async fn start() -> UnlockEnv {
        let ws = WsEnv::start_with(WsOptions { real_gate: true, ..WsOptions::default() }).await;
        let clock: Arc<dyn holzwave_server::clock::Clock> = ws.test.clock.clone();
        let accounts = Accounts::new(ws.test.store.clone(), clock, ws.test.app.audit.clone(), UID_RANGE);
        let gate = ws.gate.clone().expect("the real gate");
        UnlockEnv { ws, accounts, gate }
    }

    // ---- time ----

    /// The time of the clock of the application.
    pub fn now(&self) -> i64 {
        self.ws.test.clock.unix()
    }

    /// Moves the clock of the application forward.
    pub fn advance(&self, seconds: i64) {
        self.ws.test.clock.advance(seconds);
    }

    // ---- requests ----

    fn base(method: &str, path: &str) -> Request {
        Request::new(method, path)
            .header("Origin", ORIGIN)
            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")
            .close()
    }

    /// A request with a JSON body and no session.
    pub async fn post(&self, path: &str, body: &Value, cookie: Option<(&str, &str)>) -> Reply {
        let mut request = UnlockEnv::base("POST", path).header("Content-Type", "application/json");
        request.body = serde_json::to_vec(body).expect("json");
        if let Some((name, value)) = cookie {
            request = request.header("Cookie", &format!("{name}={value}"));
        }
        self.ws.plain(request).await
    }

    /// A request of a signed-in browser: the session cookie and the anti-forgery token.
    pub async fn api(&self, who: &Signed, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let mut request = UnlockEnv::base(method, path).cookie(&who.cookie).header("X-CSRF-Token", &who.csrf);
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json");
            request.body = serde_json::to_vec(body).expect("json");
        }
        self.ws.plain(request).await
    }

    /// A request of the administrator or of a person who is signed in, with an empty object as the body.
    pub async fn act(&self, who: &Signed, method: &str, path: &str) -> Reply {
        self.api(who, method, path, Some(&json!({}))).await
    }

    // ---- people ----

    fn cookie_of(reply: &Reply, name: &str) -> Option<String> {
        reply
            .all("set-cookie")
            .iter()
            .find_map(|c| c.strip_prefix(&format!("{name}=")).map(|rest| rest.split(';').next().unwrap_or_default().to_owned()))
            .filter(|value| !value.is_empty())
    }

    fn code(&self, secret: &[u8]) -> String {
        holzwave_server::auth::totp::code_at(secret, self.now()).expect("a code").to_string()
    }

    /// The `Signed` value of a session cookie: the row and the anti-forgery token.
    pub fn signed_from(&self, cookie: &str) -> Signed {
        let session = self.ws.test.sessions.lookup(cookie, self.now()).expect("the session is alive");
        let csrf = self.ws.test.app.csrf_token(&session);
        Signed { cookie: cookie.to_owned(), account: session.account_id, session, csrf }
    }

    async fn enrol_through(&self, route: &str, token: &str, name: &str, password: &str) -> (User, Signed, Reply) {
        let begin = self
            .post(
                route,
                &json!({"step": "begin", "token": token, "username": name, "display_name": name, "method": "password", "password": password}),
                None,
            )
            .await;
        assert_eq!(begin.status, 200, "begin: {}", begin.text());
        let pending = UnlockEnv::cookie_of(&begin, "__Host-hw_pending").expect("a pending cookie");
        let secret = begin.json()["secret"].as_str().expect("a secret").to_owned();
        let secret = totp_rs::Secret::try_from_base32(secret).expect("base32").as_bytes().to_vec();
        let finish = self
            .post(route, &json!({"step": "finish", "code": self.code(&secret)}), Some(("__Host-hw_pending", &pending)))
            .await;
        assert_eq!(finish.status, 200, "finish: {}", finish.text());
        let cookie = UnlockEnv::cookie_of(&finish, "__Host-hw_session").expect("a session cookie");
        let recovery = finish.json()["recovery_code"].as_str().expect("a recovery code").to_owned();
        let account = self.ws.test.store.account_by_username(name).expect("read").expect("the account");
        let user = User {
            name: name.to_owned(),
            password: password.to_owned(),
            secret,
            id: account.id,
            key: AccountKey { id: account.id, uid: account.uid },
            recovery,
        };
        let signed = self.signed_from(&cookie);
        (user, signed, finish)
    }

    /// The first administrator, through the bootstrap route. Answers the person, the session the enrolment made and the
    /// `finish` answer.
    pub async fn bootstrap(&self, name: &str, password: &str) -> (User, Signed, Reply) {
        let token = self.accounts.bootstrap_issue().expect("a token").token.expose().to_owned();
        self.enrol_through("/auth/bootstrap", &token, name, password).await
    }

    /// A member invited by `admin`, through the invitation route.
    pub async fn invite(&self, admin: &User, name: &str, password: &str) -> (User, Signed, Reply) {
        let token = self.accounts.invite_create(admin.id, None, None, Role::Member).expect("an invitation").token.expose().to_owned();
        self.enrol_through("/auth/invite/redeem", &token, name, password).await
    }

    /// Alice (the administrator) and bob, both enrolled and signed in with the session of their enrolment. Answers the two
    /// people, their sessions, and the audit rows the enrolments wrote.
    pub async fn people(&self) -> (User, Signed, User, Signed) {
        let (alice, alice_session, _) = self.bootstrap("alice", PASSWORD).await;
        let (bob, bob_session, _) = self.invite(&alice, "bob", "another horse battery").await;
        (alice, alice_session, bob, bob_session)
    }

    /// Signs the person in with the password and the authenticator code through the real routes. Each sign-in moves the clock by
    /// [`CODE_STEP`] so that its authenticator code is a new one.
    pub async fn sign_in(&self, user: &User) -> Signed {
        self.advance(CODE_STEP);
        let begin = self.post("/auth/password", &json!({"username": user.name, "password": user.password}), None).await;
        assert_eq!(begin.status, 200, "the password step: {}", begin.text());
        let pending = UnlockEnv::cookie_of(&begin, "__Host-hw_pending").expect("a pending cookie");
        let finish = self.post("/auth/totp", &json!({"code": self.code(&user.secret)}), Some(("__Host-hw_pending", &pending))).await;
        assert_eq!(finish.status, 200, "the code step: {}", finish.text());
        let cookie = UnlockEnv::cookie_of(&finish, "__Host-hw_session").expect("a session cookie");
        self.signed_from(&cookie)
    }

    /// Signs out the session with `POST /auth/logout`.
    pub async fn sign_out(&self, who: &Signed) -> Reply {
        self.act(who, "POST", "/auth/logout").await
    }

    /// Opens a WebSocket for the session.
    pub async fn socket(&self, who: &Signed) -> WsClient {
        self.ws.connect_as(who).await
    }

    // ---- the store of a person ----

    /// What the application thinks of the store of the person right now.
    pub fn state(&self, user: &User) -> StoreState {
        self.gate.state(user.id)
    }

    /// Waits until the store of the person is in `want`.
    pub async fn wait_state(&self, user: &User, want: StoreState, what: &str) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while self.state(user) != want {
            assert!(tokio::time::Instant::now() < deadline, "{what}: the store did not become {want:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The home of the person.
    pub fn home(&self, user: &User) -> PathBuf {
        self.ws.launcher.home(user.key.uid)
    }

    /// The path of the store file of the person.
    pub fn store_file(&self, user: &User) -> PathBuf {
        store_path(&self.home(user))
    }

    /// The SHA-256 of the store file, or `None` when there is none.
    pub fn store_sha256(&self, user: &User) -> Option<String> {
        let bytes = std::fs::read(self.store_file(user)).ok()?;
        Some(hex(&Sha256::digest(&bytes)))
    }

    /// The bytes of the store file.
    pub fn store_bytes(&self, user: &User) -> Option<Vec<u8>> {
        std::fs::read(self.store_file(user)).ok()
    }

    /// One look of the gate at every open store, at the time of the clock.
    pub async fn sweep(&self) -> usize {
        self.gate.sweep_locks(self.now()).await
    }

    /// Waits until `check` holds.
    pub async fn eventually(&self, what: &str, check: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !check() {
            assert!(tokio::time::Instant::now() < deadline, "{what} did not happen in time");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // ---- the audit log ----

    /// Every row of the audit log, oldest first.
    pub fn audit_lines(&self) -> Vec<Row> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.ws.test.audit_dir)
            .map(|dir| dir.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        let mut rows = Vec::new();
        for file in files {
            for line in std::fs::read_to_string(&file).unwrap_or_default().lines() {
                let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("an audit line is JSON ({e}): {line}"));
                rows.push(Row {
                    action: v["action"].as_str().unwrap_or_default().to_owned(),
                    result: v["result"].as_str().unwrap_or_default().to_owned(),
                    actor: v["actor"].as_str().unwrap_or_default().to_owned(),
                    target: v["target"].as_str().unwrap_or_default().to_owned(),
                    detail: v["detail"].clone(),
                });
            }
        }
        rows
    }

    /// The rows written after the first `mark` rows.
    pub fn audit_since(&self, mark: usize) -> Vec<Row> {
        self.audit_lines().into_iter().skip(mark).collect()
    }

    /// The rows of one action.
    pub fn audit_of(&self, action: &str) -> Vec<Row> {
        self.audit_lines().into_iter().filter(|r| r.action == action).collect()
    }

    /// The raw text of the audit log.
    pub fn audit_text(&self) -> String {
        let mut text = String::new();
        if let Ok(dir) = std::fs::read_dir(&self.ws.test.audit_dir) {
            for entry in dir.flatten() {
                text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        text
    }

    // ---- commands ----

    /// Sends one command as the person's own socket straight into the worker, with no session: the way a command arrives
    /// whatever the sessions are. Answers the JSON of the answer.
    pub async fn direct(&self, user: &User, command: &str, args: &Value) -> Result<Value, AttachError> {
        let (handle, mut rx) = self.ws.supervisor.attach(user.key).await?;
        let id = 900_000;
        handle.submit(id, command, &args.to_string()).expect("the command is queued");
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let got = tokio::time::timeout(left, rx.recv()).await.expect("an answer in time").expect("the socket stays open");
            if let Outbound::Text(text) = got {
                let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    return Ok(value);
                }
            }
        }
    }

    /// The fault text of an answer, or `None` for a success.
    pub fn fault_text(answer: &Value) -> Option<String> {
        answer.get("err").map(|e| e["text"].as_str().map_or_else(|| e.to_string(), str::to_owned))
    }

    /// The root of the data volume: the directories the server and its workers write into.
    pub fn volume(&self) -> Vec<PathBuf> {
        let home = self.ws.launcher.home(20000);
        let workers = home.parent().and_then(Path::parent).expect("the root of the homes").to_path_buf();
        let server = self.ws.test.audit_dir.parent().and_then(Path::parent).expect("the data directory").to_path_buf();
        vec![server, workers]
    }
}

/// Saves an assistant profile over the socket and answers its id.
pub async fn ai_profile(socket: &mut WsClient) -> String {
    socket
        .ok("ai_profile_save", r#"{"input":{"name":"Cloud","preset":"anthropic"}}"#)
        .await
        .as_str()
        .expect("an id")
        .to_owned()
}

/// Sets an assistant key over the socket; the fault text on a refusal.
pub async fn ai_key_set(socket: &mut WsClient, profile: &str, key: &str) -> Result<(), String> {
    let args = json!({"profile": profile, "slot": "provider", "api_key": key}).to_string();
    let answer = socket.call("ai_key_set", &args).await;
    match UnlockEnv::fault_text(&answer) {
        Some(text) => Err(text),
        None => Ok(()),
    }
}

/// Whether a key is saved for the profile; the fault text when the store is locked.
pub async fn ai_has_key(socket: &mut WsClient, profile: &str) -> Result<bool, String> {
    let args = json!({"profile": profile, "slot": "provider"}).to_string();
    let answer = socket.call("ai_has_key", &args).await;
    match UnlockEnv::fault_text(&answer) {
        Some(text) => Err(text),
        None => Ok(answer["ok"].as_bool().unwrap_or(false)),
    }
}

/// Saves an integration profile with these secret variables.
pub async fn integration_profile(socket: &mut WsClient, id: &str, names: &[String]) {
    let args = json!({"profile": {
        "id": id, "tool": "kube", "name": "Production", "context": "prod-cluster", "production": true,
        "isolated": false, "env": {}, "secret_env": names}});
    let answer = socket.call("integration_profile_save", &args.to_string()).await;
    assert!(answer.get("ok").is_some(), "the profile is saved: {answer}");
}

/// Sets one integration secret; the fault text on a refusal.
pub async fn integration_secret_set(socket: &mut WsClient, profile: &str, name: &str, value: &str) -> Result<(), String> {
    let args = json!({"profile": profile, "name": name, "value": value}).to_string();
    let answer = socket.call("integration_secret_set", &args).await;
    match UnlockEnv::fault_text(&answer) {
        Some(text) => Err(text),
        None => Ok(()),
    }
}

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The five forms in which a text could appear in a file: as it is, in base64 and base64url (padded or not, which share the
/// prefix of the unpadded form), in hexadecimal and percent-encoded.
pub fn encodings(text: &str) -> Vec<(&'static str, Vec<u8>)> {
    let bytes = text.as_bytes();
    let mut forms: Vec<(&'static str, Vec<u8>)> = vec![("plain", bytes.to_vec())];
    forms.push(("base64", base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes).into_bytes()));
    forms.push(("base64url", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes).into_bytes()));
    forms.push(("hex", hex(bytes).into_bytes()));
    forms.push(("percent", percent(bytes).into_bytes()));
    forms
}

fn percent(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Every regular file below `root`, links not followed.
pub fn files_below(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => found.push(path),
                _ => {}
            }
        }
    }
    found.sort();
    found
}

/// Reads every file below the roots as bytes and reports each place where a needle is found in any of the five encodings. A
/// needle is given as its name and its text; the hit names the needle, never shows it. The number of files read is returned too.
pub fn scan_volume_for(roots: &[PathBuf], needles: &[(&str, &str)]) -> (Vec<Hit>, usize) {
    let forms: Vec<(String, &'static str, Vec<u8>)> = needles
        .iter()
        .flat_map(|(name, text)| encodings(text).into_iter().map(move |(how, bytes)| ((*name).to_owned(), how, bytes)))
        .collect();
    let mut hits = Vec::new();
    let mut read = 0;
    for root in roots {
        for file in files_below(root) {
            let Ok(bytes) = std::fs::read(&file) else { continue };
            read += 1;
            for (name, how, form) in &forms {
                if let Some(offset) = find(&bytes, form) {
                    hits.push(Hit { path: file.clone(), offset, needle: format!("{name} ({how})") });
                }
            }
        }
    }
    (hits, read)
}

/// The scan on text that is not a file (a log capture, a `Debug` text): the names of the needles it contains.
pub fn scan_text_for(text: &str, needles: &[(&str, &str)]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, needle) in needles {
        for (how, form) in encodings(needle) {
            if find(text.as_bytes(), &form).is_some() {
                found.push(format!("{name} ({how})"));
            }
        }
    }
    found
}
