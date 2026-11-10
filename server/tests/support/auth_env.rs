//! The test environment of the sign-in routes: every sign-in service over one scratch store, a manual clock, a software
//! authenticator, the in-process identity provider, a recording fake of the unlock gate, and the router built from the parts
//! of `routes_auth` and `routes_unlock`. The route tests of the account-creation and account plans reuse it with
//! `#[path = "support/auth_env.rs"] mod auth_env;` next to `#[path = "support/edge.rs"] mod edge;`.
//!
//! Requests go to the router as a `tower` service, as in `edge.rs`; no socket is opened.
#![allow(dead_code)]

#[path = "oidc_provider.rs"]
pub mod oidc_provider;
#[path = "scratch.rs"]
pub mod scratch;
#[path = "webauthn_client.rs"]
pub mod webauthn_client;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::Method;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::accounts::Accounts;
use holzwave_server::audit::Audit;
use holzwave_server::auth::oidc::OidcService;
use holzwave_server::auth::passkey::{Passkeys, RegistrationSubject};
use holzwave_server::auth::password::{ArgonGate, enrol_password};
use holzwave_server::auth::session::{Method as SignInMethod, SessionManager};
use holzwave_server::auth::signin::SignIn;
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock, ManualClock};
use holzwave_server::config::{self, Auth, Sessions};
use holzwave_server::http::app::{App, AppInit, PublicOrigin, StoreState, StoreView};
use holzwave_server::http::router;
use holzwave_server::http::routes_auth::{AuthServices, LandingPage, OidcFlow};
use holzwave_server::http::routes_unlock::{GateError, GateFuture, Material, UnlockGate, UnlockSecret};
use holzwave_server::http::{routes_auth, routes_unlock};
use holzwave_server::limits::Limits;
use holzwave_server::proxy::{Mode, TrustedProxies};
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, NewAccount, Role, Store};
use holzwave_server::unlock::Kdf;
use oidc_provider::{CLIENT_ID, Callback, ISSUER, MemoryHttp, SECRET, TestProvider};
use scratch::Scratch;
use serde_json::Value;
use tower::ServiceExt as _;
use webauthn_client::SoftPasskey;
use webauthn_rs::prelude::Url;
use zeroize::Zeroizing;

use crate::edge::{HOST, NOW, ORIGIN, Req, Resp, Signed, UID_RANGE, server_key};

/// The public address of the test server.
pub const PUBLIC_URL: &str = "https://hw.example.test";

/// The client address of the direct service calls.
pub const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

/// The password of every test account that has one.
pub const PASSWORD: &str = "correct horse battery";

/// A wrong password.
pub const WRONG: &str = "wrong horse battery staple";

/// The browser every test sign-in comes from.
pub const FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

/// The recovery code and the passphrase the fake gate accepts.
pub const PASSPHRASE: &str = "a long passphrase for the store";
pub const RECOVERY: &str = "ABCD-EFGH-JKLM-NPQR-STUV-WXYZ-2345";

/// The login page the callback of a successful single sign-on answers with.
pub const LOGIN_PAGE: &str = "<!doctype html><title>holzWave</title><script src=\"/login.js\"></script>";

/// Which sign-in methods are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Methods {
    pub passkey: bool,
    pub password: bool,
    pub oidc: bool,
}

impl Methods {
    /// All three.
    pub const ALL: Methods = Methods { passkey: true, password: true, oidc: true };

    /// The seven combinations with at least one method on.
    pub fn combinations() -> Vec<Methods> {
        let mut all = Vec::new();
        for bits in 1u8..8 {
            all.push(Methods { passkey: bits & 1 != 0, password: bits & 2 != 0, oidc: bits & 4 != 0 });
        }
        all
    }

    /// A short label for a failure message.
    pub fn label(&self) -> String {
        format!(
            "passkey={} password={} oidc={}",
            u8::from(self.passkey),
            u8::from(self.password),
            u8::from(self.oidc)
        )
    }
}

/// What a test may change about the services.
#[derive(Clone)]
pub struct Tuning {
    pub limits: Limits,
    pub permits: u32,
    pub kdf: Kdf,
    pub max_pending: usize,
    /// A pause at the start of every derivation, to make a derivation take a known time.
    pub derive_pause: Option<Duration>,
    /// The operator's ceiling over the device features and the idle time of a device token.
    pub devices: holzwave_server::http::features::DeviceSettings,
}

impl Default for Tuning {
    fn default() -> Tuning {
        Tuning {
            limits: Limits::DEFAULT,
            permits: 2,
            kdf: Kdf::lower_bound(),
            max_pending: holzwave_server::auth::signin::MAX_PENDING,
            derive_pause: None,
            devices: holzwave_server::http::features::DeviceSettings::default(),
        }
    }
}

/// One call of `session_started` as the fake gate saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub account: AccountId,
    pub kind: &'static str,
    /// The key bytes the gate was handed (empty for no material). Only a test can read them.
    pub key: Vec<u8>,
}

/// One call of `unlock_with`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockCall {
    pub account: AccountId,
    pub kind: &'static str,
    pub secret: String,
}

/// A recording fake of the unlock gate. It accepts [`PASSPHRASE`] and [`RECOVERY`] and refuses everything else.
#[derive(Default)]
pub struct FakeGate {
    started: Mutex<Vec<Call>>,
    unlocks: Mutex<Vec<UnlockCall>>,
    locked: Mutex<HashSet<i64>>,
    /// `session_started` fails when set.
    pub fail_started: AtomicBool,
    /// `unlock_with` answers `Unavailable` when set.
    pub unavailable: AtomicBool,
}

impl FakeGate {
    pub fn started(&self) -> Vec<Call> {
        self.started.lock().expect("lock").clone()
    }

    pub fn unlocks(&self) -> Vec<UnlockCall> {
        self.unlocks.lock().expect("lock").clone()
    }

    pub fn set_locked(&self, account: AccountId, locked: bool) {
        let mut set = self.locked.lock().expect("lock");
        if locked {
            set.insert(account.get());
        } else {
            set.remove(&account.get());
        }
    }
}

impl StoreView for FakeGate {
    fn state(&self, account: AccountId) -> StoreState {
        if self.locked.lock().expect("lock").contains(&account.get()) { StoreState::Locked } else { StoreState::Open }
    }
}

impl UnlockGate for FakeGate {
    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>> {
        let key = match &material {
            Material::Password(m) => m.kek().expose().to_vec(),
            Material::Passkey(m) => m.output().expose().to_vec(),
            Material::None => Vec::new(),
        };
        self.started.lock().expect("lock").push(Call { account, kind: material.kind(), key });
        let fail = self.fail_started.load(Ordering::SeqCst);
        Box::pin(async move { if fail { Err(GateError::Unavailable) } else { Ok(()) } })
    }

    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>> {
        let (kind, text, right) = match secret {
            UnlockSecret::Passphrase(s) => ("passphrase", s.to_owned(), s == PASSPHRASE),
            UnlockSecret::RecoveryCode(s) => ("recovery_code", s.to_owned(), s == RECOVERY),
        };
        self.unlocks.lock().expect("lock").push(UnlockCall { account, kind, secret: text });
        let unavailable = self.unavailable.load(Ordering::SeqCst);
        if right {
            self.set_locked(account, false);
        }
        Box::pin(async move {
            if unavailable {
                Err(GateError::Unavailable)
            } else if right {
                Ok(())
            } else {
                Err(GateError::Refused)
            }
        })
    }
}

/// The login page of the tests.
pub struct FixedPage;

impl LandingPage for FixedPage {
    fn login_page(&self) -> Bytes {
        Bytes::from_static(LOGIN_PAGE.as_bytes())
    }
}

/// The environment.
pub struct AuthEnv {
    pub scratch: Scratch,
    pub methods: Methods,
    pub store: Arc<Store>,
    pub clock: Arc<ManualClock>,
    pub sessions: Arc<SessionManager>,
    pub app: Arc<App>,
    pub argon: ArgonGate,
    pub signin: Arc<SignIn>,
    pub passkeys: Arc<Passkeys>,
    pub provider: Option<Arc<TestProvider>>,
    pub oidc: Option<Arc<dyn OidcFlow>>,
    pub gate: Arc<FakeGate>,
    pub origin: Url,
    sealer: Arc<Sealer>,
    secrets: Mutex<HashMap<String, Zeroizing<[u8; totp::SECRET_BYTES]>>>,
    handles: Mutex<HashMap<String, [u8; 16]>>,
    router: Router,
}

fn provider_entry(secret_file: PathBuf) -> config::Oidc {
    config::Oidc {
        id: "corp".to_owned(),
        name: "Company".to_owned(),
        issuer: ISSUER.to_owned(),
        client_id: CLIENT_ID.to_owned(),
        client_secret_file: secret_file,
        allowed_domains: vec!["example.org".to_owned()],
        allowed_groups: Vec::new(),
        groups_claim: None,
    }
}

impl AuthEnv {
    /// An environment with the given methods on and the default tuning.
    pub fn new(methods: Methods) -> AuthEnv {
        AuthEnv::build(methods, Tuning::default())
    }

    /// An environment with everything on.
    pub fn all() -> AuthEnv {
        AuthEnv::new(Methods::ALL)
    }

    /// An environment with the given methods and tuning.
    pub fn build(methods: Methods, tuning: Tuning) -> AuthEnv {
        let scratch = Scratch::new("auth");
        let clock = Arc::new(ManualClock::new(NOW));
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
        let audit = Arc::new(Audit::open(scratch.path(), dyn_clock.clone()).expect("open the audit log"));
        let sessions = Arc::new(SessionManager::new(store.clone(), dyn_clock.clone(), Sessions::default()));
        let mut argon = ArgonGate::new(tuning.permits, tuning.kdf);
        if let Some(pause) = tuning.derive_pause {
            argon = argon.with_hook(Arc::new(move || std::thread::sleep(pause)));
        }
        let sealer = Arc::new(Sealer::new(&server_key()));
        let signin = Arc::new(
            SignIn::new(store.clone(), dyn_clock.clone(), audit.clone(), sessions.clone(), argon.clone(), sealer.clone())
                .with_limits(tuning.limits)
                .with_max_pending(tuning.max_pending),
        );
        let passkeys = Arc::new(
            Passkeys::new(PUBLIC_URL, "holzWave", store.clone(), sessions.clone(), audit.clone(), dyn_clock.clone())
                .expect("the relying party builds"),
        );
        let gate = Arc::new(FakeGate::default());

        let (provider, oidc, providers) = if methods.oidc {
            let provider = TestProvider::standard(clock.clone());
            let secret = scratch.write_file("client-secret", format!("{SECRET}\n").as_bytes(), 0o600);
            let accounts = Arc::new(Accounts::new(store.clone(), dyn_clock.clone(), audit.clone(), UID_RANGE));
            let service = OidcService::new(
                &[provider_entry(secret.clone())],
                PUBLIC_URL,
                accounts,
                sessions.clone(),
                audit.clone(),
                dyn_clock.clone(),
                MemoryHttp::new(std::slice::from_ref(&provider)),
            )
            .expect("the single sign-on service builds");
            let flow: Arc<dyn OidcFlow> = Arc::new(service);
            (Some(provider), Some(flow), vec![provider_entry(secret)])
        } else {
            (None, None, Vec::new())
        };

        let app = App::new(
            AppInit {
                origin: PublicOrigin::parse(PUBLIC_URL).expect("a public url"),
                auth: Auth { passkey: methods.passkey, password_totp: methods.password, oidc: providers },
                sessions: Sessions::default(),
                limits: tuning.limits,
                mode: Mode::Files,
                proxies: TrustedProxies::none(),
                store: store.clone(),
                session_manager: sessions.clone(),
                audit,
                clock: dyn_clock,
                view: gate.clone(),
                devices: tuning.devices,
            },
            &server_key(),
        );
        let services = Arc::new(AuthServices {
            app: app.clone(),
            signin: signin.clone(),
            passkeys: passkeys.clone(),
            oidc: oidc.clone(),
            gate: gate.clone(),
            page: Arc::new(FixedPage),
        });
        let mut parts = routes_auth::parts(services.clone());
        parts.extend(routes_unlock::parts(services));
        let router = router::build(app.clone(), parts);
        AuthEnv {
            scratch,
            methods,
            store,
            clock,
            sessions,
            app,
            argon,
            signin,
            passkeys,
            provider,
            oidc,
            gate,
            origin: Url::parse(PUBLIC_URL).expect("origin parses"),
            sealer,
            secrets: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            router,
        }
    }

    // ---- requests ----

    /// Sends a request to the router.
    pub async fn send(&self, req: Req) -> Resp {
        let response = self.router.clone().oneshot(req.build()).await.expect("the router never fails");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("the body can be read");
        Resp { status: parts.status, headers: parts.headers, body: bytes.to_vec() }
    }

    /// A read as the page sends it.
    pub fn get(&self, path: &str) -> Req {
        Req::new(Method::GET, path).header("host", HOST).header("user-agent", FIREFOX)
    }

    /// A change as the page sends it before a session exists: the host, the origin and the JSON content type.
    pub fn post(&self, path: &str, body: &Value) -> Req {
        Req::new(Method::POST, path)
            .header("host", HOST)
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .header("user-agent", FIREFOX)
            .json(body)
    }

    /// A change with a raw body, as a hostile client sends it. The content type is a parameter.
    pub fn post_raw(&self, path: &str, body: Vec<u8>, content_type: &str) -> Req {
        Req::new(Method::POST, path)
            .header("host", HOST)
            .header("origin", ORIGIN)
            .header("content-type", content_type)
            .header("user-agent", FIREFOX)
            .bytes(body)
    }

    /// A change with a session: the cookie (and a pending or transaction cookie when given) and the token.
    pub fn post_as(&self, path: &str, body: &Value, who: &Signed) -> Req {
        self.post(path, body).set_cookies(&[("__Host-hw_session", &who.cookie)]).csrf(&who.csrf)
    }

    // ---- accounts ----

    /// An account with a password and an authenticator.
    pub async fn person(&self, name: &str) -> AccountId {
        let id = self.bare(name, Role::Member);
        self.set_password(id).await;
        self.set_authenticator(name, id);
        id
    }

    /// An account with nothing to sign in with yet.
    pub fn bare(&self, name: &str, role: Role) -> AccountId {
        let mut handle = [0u8; 16];
        getrandom::fill(&mut handle).expect("random");
        self.handles.lock().expect("lock").insert(name.to_owned(), handle);
        let now = self.clock.unix();
        self.store.write(|tx| tx.account_create(&NewAccount::new(name, role).handle(handle), UID_RANGE, now)).expect("account").id
    }

    pub async fn set_password(&self, id: AccountId) {
        enrol_password(&self.argon, &self.store, id, PASSWORD, self.clock.unix()).await.expect("password");
    }

    /// Gives the account a password of the given text.
    pub async fn set_password_text(&self, id: AccountId, text: &str) {
        enrol_password(&self.argon, &self.store, id, text, self.clock.unix()).await.expect("password");
    }

    pub fn set_authenticator(&self, name: &str, id: AccountId) {
        let secret = totp::generate_secret().expect("secret");
        let sealed = self.sealer.seal(&totp::seal_context(id), &secret[..]).expect("seal");
        self.store.write(|tx| tx.totp_set(id, &sealed)).expect("store the secret");
        self.secrets.lock().expect("lock").insert(name.to_owned(), secret);
    }

    /// The code the person's app shows now.
    pub fn code(&self, name: &str) -> String {
        let secrets = self.secrets.lock().expect("lock");
        let secret = secrets.get(name).expect("a secret");
        totp::code_at(&secret[..], self.clock.unix()).expect("code").to_string()
    }

    /// A six-digit code that is none of the three the window accepts.
    pub fn wrong_code(&self, name: &str) -> String {
        let secrets = self.secrets.lock().expect("lock");
        let secret = secrets.get(name).expect("a secret");
        let now = self.clock.unix();
        let accepted: Vec<String> =
            [-30, 0, 30].iter().map(|d| totp::code_at(&secret[..], now + d).expect("code").to_string()).collect();
        (0..1_000_000).map(|n| format!("{n:06}")).find(|c| !accepted.contains(c)).expect("a code")
    }

    /// Registers a passkey for the account `name` (which must exist) and answers the authenticator.
    pub fn register_passkey(&self, name: &str) -> SoftPasskey {
        let handle = *self.handles.lock().expect("lock").get(name).expect("an account made by this environment");
        let subject = RegistrationSubject { handle, user_name: name.to_owned(), display_name: name.to_owned() };
        let mut authenticator = SoftPasskey::new();
        let (options, token) = self.passkeys.start_registration(&subject, ADDR).expect("registration starts");
        let options: Value = serde_json::from_str(&options).expect("options are JSON");
        let response = authenticator.register(&options, &self.origin).expect("the authenticator answers");
        self.passkeys
            .finish_registration(token.expose(), &response.to_string(), None, "Key", ADDR)
            .expect("the registration verifies");
        authenticator
    }

    /// An account with a passkey and nothing else.
    pub fn passkey_person(&self, name: &str) -> (AccountId, SoftPasskey) {
        let id = self.bare(name, Role::Member);
        (id, self.register_passkey(name))
    }

    /// A session made directly by the manager (no route), with the given method.
    pub fn session_for(&self, name: &str, method: SignInMethod) -> Signed {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        let created = self
            .sessions
            .create(account.id, method, self.clock.unix(), "Firefox on Linux", Some(ADDR))
            .expect("a session");
        let csrf = self.app.csrf_token(&created.session);
        Signed { cookie: created.value.expose().to_owned(), session: created.session, account: account.id, csrf }
    }

    // ---- whole flows through the router ----

    /// Password and code through the router; answers the signed-in person.
    pub async fn password_sign_in(&self, name: &str) -> Signed {
        let first = self.send(self.post("/auth/password", &serde_json::json!({"username": name, "password": PASSWORD}))).await;
        assert_eq!(first.status, 200, "the first step: {}", first.text());
        let pending = set_cookie_value(&first, "__Host-hw_pending").expect("a pending cookie");
        let second = self
            .send(self.post("/auth/totp", &serde_json::json!({"code": self.code(name)})).set_cookies(&[("__Host-hw_pending", &pending)]))
            .await;
        assert_eq!(second.status, 200, "the second step: {}", second.text());
        self.signed_from(&second)
    }

    /// The signed-in person a sign-in answer set the cookie for.
    pub fn signed_from(&self, response: &Resp) -> Signed {
        let cookie = set_cookie_value(response, "__Host-hw_session").expect("a session cookie");
        let session = self.sessions.lookup(&cookie, self.clock.unix()).expect("the session is alive");
        let csrf = self.app.csrf_token(&session);
        Signed { account: session.account_id, cookie, session, csrf }
    }

    /// The options of a passkey sign-in and the pending cookie value.
    pub async fn passkey_begin(&self) -> (Resp, Value, String) {
        let response = self.send(self.post("/auth/passkey/begin", &serde_json::json!({}))).await;
        assert_eq!(response.status, 200, "{}", response.text());
        let body = response.json();
        let options = body["options"].clone();
        let pending = set_cookie_value(&response, "__Host-hw_pending").expect("a pending cookie");
        (response, options, pending)
    }

    /// A passkey sign-in through the router with the authenticator; `prf` is what the page sends beside the credential.
    pub async fn passkey_sign_in(&self, authenticator: &mut SoftPasskey, prf: Option<&str>) -> Resp {
        let (_, options, pending) = self.passkey_begin().await;
        let credential = authenticator.authenticate(&options, &self.origin).expect("the authenticator answers");
        let mut body = serde_json::json!({ "credential": credential });
        if let Some(prf) = prf {
            body["prf"] = serde_json::json!(prf);
        }
        self.send(self.post("/auth/passkey/finish", &body).set_cookies(&[("__Host-hw_pending", &pending)])).await
    }

    /// The start of a single sign-on, the provider's step and the callback request. Answers the start response, the callback
    /// parameters and the transaction cookie value.
    pub async fn oidc_begin(&self) -> (Resp, Callback, String) {
        let start = self.send(self.get("/auth/oidc/corp/start")).await;
        assert_eq!(start.status, 302, "{}", start.text());
        let location = start.header("location").expect("a location");
        let callback = self.provider.as_ref().expect("a provider").authorize(&location);
        let transaction = set_cookie_value(&start, "__Host-hw_oidc").expect("a transaction cookie");
        (start, callback, transaction)
    }

    /// The callback request of the browser with the given parameters and cookie.
    pub fn oidc_callback_request(&self, code: &str, state: &str, transaction: Option<&str>) -> Req {
        let req = self.get(&format!("/auth/oidc/callback?code={code}&state={state}"));
        match transaction {
            Some(value) => req.set_cookies(&[("__Host-hw_oidc", value)]),
            None => req,
        }
    }

    /// A single sign-on through the router; answers the callback response.
    pub async fn oidc_sign_in(&self) -> Resp {
        let (_, callback, transaction) = self.oidc_begin().await;
        self.send(self.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await
    }

    // ---- what the run left behind ----

    /// Every audit line of every day, in order.
    pub fn audit_lines(&self) -> Vec<Value> {
        let dir = self.scratch.path().join("server/audit");
        let mut files: Vec<_> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries.map(|e| e.expect("entry").path()).collect(),
            Err(_) => return Vec::new(),
        };
        files.sort();
        files
            .iter()
            .flat_map(|f| std::fs::read_to_string(f).expect("read an audit file").lines().map(str::to_owned).collect::<Vec<_>>())
            .map(|l| serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
            .collect()
    }

    /// How many audit lines have the action.
    pub fn count(&self, action: &str) -> usize {
        self.audit_lines().iter().filter(|l| l["action"] == action).count()
    }

    /// The whole audit directory as text.
    pub fn audit_text(&self) -> String {
        let dir = self.scratch.path().join("server/audit");
        let mut text = String::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries {
                text.push_str(&std::fs::read_to_string(entry.expect("entry").path()).unwrap_or_default());
            }
        }
        text
    }

    /// The failures counted under a throttle key now.
    pub fn failures(&self, key: &str) -> i64 {
        self.store.read(|tx| tx.throttle_get(key, self.clock.unix(), 900)).expect("throttle").failures
    }

    /// Changes the state of an account.
    pub fn set_state(&self, name: &str, state: holzwave_server::store::AccountState) {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        self.store.write(|tx| tx.account_set_state(account.id, state)).expect("set the state");
    }

    /// The number of session rows an account has.
    pub fn session_rows(&self, id: AccountId) -> usize {
        self.store.read(|tx| tx.session_list(id)).expect("list").len()
    }
}

// ---- cookies ----

/// The value a response sets for the cookie `name`, or `None` (an empty value, a deleting form, counts as `Some("")`).
pub fn set_cookie_value(response: &Resp, name: &str) -> Option<String> {
    response.all("set-cookie").iter().find_map(|line| {
        let (pair, _) = line.split_once(';').unwrap_or((line.as_str(), ""));
        let (n, v) = pair.split_once('=')?;
        (n == name).then(|| v.to_owned())
    })
}

/// The whole `Set-Cookie` line for the cookie `name`.
pub fn set_cookie_line(response: &Resp, name: &str) -> Option<String> {
    response.all("set-cookie").into_iter().find(|line| line.starts_with(&format!("{name}=")))
}

/// Everything that identifies a response from the outside: status, headers (name and value, sorted) and body.
pub fn fingerprint(response: &Resp) -> String {
    let mut lines: Vec<String> = response
        .headers
        .iter()
        .map(|(n, v)| format!("{}: {}", n.as_str(), String::from_utf8_lossy(v.as_bytes())))
        .collect();
    lines.sort();
    format!("{}\n{}\n\n{}", response.status, lines.join("\n"), response.text())
}

/// Adds a single `Cookie` header with the given pairs.
pub trait WithCookies {
    fn set_cookies(self, pairs: &[(&str, &str)]) -> Self;
}

impl WithCookies for Req {
    fn set_cookies(self, pairs: &[(&str, &str)]) -> Req {
        let text: Vec<String> = pairs.iter().map(|(n, v)| format!("{n}={v}")).collect();
        self.without("cookie").header("cookie", &text.join("; "))
    }
}

/// A 43-character base64url string of 32 bytes, the form of a PRF output.
pub fn prf_text(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 32])
}
