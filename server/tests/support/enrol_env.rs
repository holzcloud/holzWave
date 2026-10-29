//! The test environment of the enrolment routes: the sign-in environment, a recording gate that wraps its fake, the accounts
//! service, and a router that holds the sign-in, unlock and enrolment parts together.
//!
//! The test file declares `mod edge;`, `mod auth_env;` and `mod enrol_env;` at its root (all three with `#[path]`).
#![allow(dead_code)]

use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holzwave_server::accounts::Accounts;
use holzwave_server::clock::Clock;
use holzwave_server::http::app::{StoreState, StoreView};
use holzwave_server::http::router;
use holzwave_server::http::routes_auth::{self, AuthServices};
use holzwave_server::http::routes_enrol::{self, Enrol, EnrolServices};
use holzwave_server::http::routes_unlock::{self, GateError, GateFuture, Material, ProvisionKey, UnlockGate, UnlockSecret};
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, Role, UnlockKind};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::auth_env::webauthn_client::SoftPasskey;
use crate::auth_env::{AuthEnv, FixedPage, Methods, Tuning, WithCookies, set_cookie_value};
use crate::edge::{Req, Resp, UID_RANGE, server_key};

/// The route of the first administrator.
pub const BOOTSTRAP: &str = "/auth/bootstrap";

/// The route of an invited person.
pub const INVITE: &str = "/auth/invite/redeem";

/// The name of the pending cookie.
pub const PENDING: &str = "__Host-hw_pending";

/// The name of the session cookie.
pub const SESSION: &str = "__Host-hw_session";

/// The exact attributes of the pending cookie.
pub const PENDING_TAIL: &str = "; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=300";

/// The exact attributes of the session cookie.
pub const SESSION_TAIL: &str = "; Secure; HttpOnly; SameSite=Strict; Path=/";

/// The exact line that deletes the pending cookie.
pub const PENDING_GONE: &str = "__Host-hw_pending=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0";

/// A password that passes the policy.
pub const PASSWORD: &str = "correct horse battery";

/// One key of a `provision` call as the gate saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySeen {
    pub kind: UnlockKind,
    pub credential_id: Option<Vec<u8>>,
    pub key: Vec<u8>,
}

/// One call of `provision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionSeen {
    pub account: AccountId,
    pub keys: Vec<KeySeen>,
}

/// One call of `session_started`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedSeen {
    pub account: AccountId,
    pub kind: &'static str,
}

/// A gate that records `provision` and `session_started` and passes everything else to the fake of the sign-in environment.
pub struct RecordingGate {
    inner: Arc<crate::auth_env::FakeGate>,
    provisions: Mutex<Vec<ProvisionSeen>>,
    started: Mutex<Vec<StartedSeen>>,
    /// `provision` fails when set.
    pub fail_provision: AtomicBool,
}

impl RecordingGate {
    pub fn provisions(&self) -> Vec<ProvisionSeen> {
        self.provisions.lock().expect("lock").clone()
    }

    pub fn started(&self) -> Vec<StartedSeen> {
        self.started.lock().expect("lock").clone()
    }
}

impl StoreView for RecordingGate {
    fn state(&self, account: AccountId) -> StoreState {
        self.inner.state(account)
    }
}

impl UnlockGate for RecordingGate {
    fn provision(&self, account: AccountId, keys: Vec<ProvisionKey>) -> GateFuture<'_, Result<(), GateError>> {
        let keys = keys
            .iter()
            .map(|k| KeySeen { kind: k.kind(), credential_id: k.credential_id().map(<[u8]>::to_vec), key: k.key().to_vec() })
            .collect();
        self.provisions.lock().expect("lock").push(ProvisionSeen { account, keys });
        let fail = self.fail_provision.load(Ordering::SeqCst);
        Box::pin(async move { if fail { Err(GateError::Unavailable) } else { Ok(()) } })
    }

    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>> {
        self.started.lock().expect("lock").push(StartedSeen { account, kind: material.kind() });
        self.inner.session_started(account, material)
    }

    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>> {
        self.inner.unlock_with(account, secret)
    }
}

/// The environment.
pub struct EnrolEnv {
    pub auth: AuthEnv,
    pub gate: Arc<RecordingGate>,
    pub accounts: Arc<Accounts>,
    pub enrol: Arc<Enrol>,
    router: Router,
}

impl Deref for EnrolEnv {
    type Target = AuthEnv;

    fn deref(&self) -> &AuthEnv {
        &self.auth
    }
}

impl EnrolEnv {
    /// An environment with the password and the passkey method on.
    pub fn new() -> EnrolEnv {
        EnrolEnv::build(Methods { passkey: true, password: true, oidc: false }, Tuning::default())
    }

    /// An environment with the given methods and tuning.
    pub fn build(methods: Methods, tuning: Tuning) -> EnrolEnv {
        let auth = AuthEnv::build(methods, tuning);
        let clock: Arc<dyn Clock> = auth.clock.clone();
        let accounts = Arc::new(Accounts::new(auth.store.clone(), clock, auth.app.audit.clone(), UID_RANGE));
        let gate = Arc::new(RecordingGate {
            inner: auth.gate.clone(),
            provisions: Mutex::new(Vec::new()),
            started: Mutex::new(Vec::new()),
            fail_provision: AtomicBool::new(false),
        });
        let services = Arc::new(AuthServices {
            app: auth.app.clone(),
            signin: auth.signin.clone(),
            passkeys: auth.passkeys.clone(),
            oidc: auth.oidc.clone(),
            gate: gate.clone(),
            page: Arc::new(FixedPage),
        });
        let enrol = Enrol::new(EnrolServices {
            auth: services.clone(),
            accounts: accounts.clone(),
            argon: auth.argon.clone(),
            sealer: Arc::new(Sealer::new(&server_key())),
        });
        let mut parts = routes_auth::parts(services.clone());
        parts.extend(routes_unlock::parts(services));
        parts.extend(routes_enrol::parts(&enrol));
        let router = router::build(auth.app.clone(), parts);
        EnrolEnv { auth, gate, accounts, enrol, router }
    }

    /// Sends a request to the router that holds the enrolment routes.
    pub async fn send(&self, req: Req) -> Resp {
        let response = self.router.clone().oneshot(req.build()).await.expect("the router never fails");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("the body can be read");
        Resp { status: parts.status, headers: parts.headers, body: bytes.to_vec() }
    }

    // ---- tokens ----

    /// A new bootstrap token.
    pub fn bootstrap_token(&self) -> String {
        self.accounts.bootstrap_issue().expect("a bootstrap token").token.expose().to_owned()
    }

    /// A first administrator made directly (no route), for tests that need an administrator to invite with.
    pub fn administrator(&self, name: &str) -> AccountId {
        self.auth.bare(name, Role::Admin)
    }

    /// An invitation for a new person with `role`, made by `admin`.
    pub fn invitation(&self, admin: AccountId, role: Role) -> String {
        self.accounts.invite_create(admin, None, None, role).expect("an invitation").token.expose().to_owned()
    }

    // ---- bodies and steps ----

    /// The `begin` body of the password method.
    pub fn begin_password_body(token: &str, username: &str, display_name: &str, password: &str) -> Value {
        json!({"step": "begin", "token": token, "username": username, "display_name": display_name, "method": "password", "password": password})
    }

    /// The `begin` body of the passkey method.
    pub fn begin_passkey_body(token: &str, username: &str, display_name: &str) -> Value {
        json!({"step": "begin", "token": token, "username": username, "display_name": display_name, "method": "passkey"})
    }

    /// `begin` with the password method.
    pub async fn begin_password(&self, route: &str, token: &str, username: &str, password: &str) -> Resp {
        self.send(self.post(route, &EnrolEnv::begin_password_body(token, username, "Alice", password))).await
    }

    /// `finish` with a code, under a pending cookie.
    pub async fn finish_code(&self, route: &str, pending: &str, code: &str) -> Resp {
        self.send(self.post(route, &json!({"step": "finish", "code": code})).set_cookies(&[(PENDING, pending)])).await
    }

    /// The authenticator secret a `begin` answer showed, as bytes.
    pub fn secret_of(&self, begin: &Resp) -> Vec<u8> {
        let body = begin.json();
        let text = body["secret"].as_str().expect("a secret in the answer").to_owned();
        totp_rs::Secret::try_from_base32(text).expect("the secret is base32").as_bytes().to_vec()
    }

    /// The code an authenticator app with `secret` shows now.
    pub fn code_for(&self, secret: &[u8]) -> String {
        holzwave_server::auth::totp::code_at(secret, self.clock.unix()).expect("a code").to_string()
    }

    /// A whole password enrolment through the router: `begin`, the code, `finish`. Answers the `finish` response and the
    /// secret.
    pub async fn enrol_with_password(&self, route: &str, token: &str, username: &str) -> (Resp, Vec<u8>) {
        let begin = self.begin_password(route, token, username, PASSWORD).await;
        assert_eq!(begin.status, 200, "begin: {}", begin.text());
        let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
        let secret = self.secret_of(&begin);
        let finish = self.finish_code(route, &pending, &self.code_for(&secret)).await;
        (finish, secret)
    }
}

impl EnrolEnv {
    /// `begin` with the passkey method.
    pub async fn begin_passkey(&self, route: &str, token: &str, username: &str) -> Resp {
        self.send(self.post(route, &EnrolEnv::begin_passkey_body(token, username, "Alice"))).await
    }

    /// `finish` with a credential (and the PRF output the page read beside it), under a pending cookie.
    pub async fn finish_credential(&self, route: &str, pending: &str, credential: &Value, prf: Option<&str>) -> Resp {
        let mut body = json!({"step": "finish", "credential": credential});
        if let Some(prf) = prf {
            body["prf"] = json!(prf);
        }
        self.send(self.post(route, &body).set_cookies(&[(PENDING, pending)])).await
    }

    /// A whole passkey enrolment through the router with a fresh software authenticator. Answers the `finish` response and the
    /// authenticator.
    pub async fn enrol_with_passkey(&self, route: &str, token: &str, username: &str, prf: Option<&str>) -> (Resp, SoftPasskey) {
        let begin = self.begin_passkey(route, token, username).await;
        assert_eq!(begin.status, 200, "begin: {}", begin.text());
        let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
        let mut key = SoftPasskey::new();
        let credential = key.register(&begin.json()["options"], &self.origin).expect("the authenticator answers");
        (self.finish_credential(route, &pending, &credential, prf).await, key)
    }
}

/// The WebAuthn user handle the creation options name, as bytes.
pub fn user_handle_of(options: &Value) -> Vec<u8> {
    let id = options["publicKey"]["user"]["id"].as_str().expect("a user id in the options");
    URL_SAFE_NO_PAD.decode(id).expect("the user id is base64url")
}

impl Default for EnrolEnv {
    fn default() -> EnrolEnv {
        EnrolEnv::new()
    }
}

/// Whether `text` is a recovery code: eight groups of four Crockford characters.
pub fn is_recovery_code(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 8
        && groups.iter().all(|g| g.len() == 4 && g.bytes().all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b)))
}
