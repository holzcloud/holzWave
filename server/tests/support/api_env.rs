//! The test environment of the account and administration routes: the sign-in environment, a recording gate that is also the
//! seam to the secret store, the accounts service, the enrolment routes (so that an invitation link can be redeemed), and a
//! router that holds the sign-in, unlock, enrolment, `/api/admin` and `/api/me` parts together.
//!
//! The test file declares `mod edge;`, `mod auth_env;` and `mod api_env;` at its root (all three with `#[path]`).
#![allow(dead_code)]

use std::ops::Deref;
use std::sync::Arc;

use axum::Router;
use axum::http::Method;
use holzwave_server::accounts::Accounts;
use holzwave_server::auth::session::Method as SignInMethod;
use holzwave_server::clock::Clock;
use holzwave_server::http::app::{StoreState, StoreView};
use holzwave_server::http::router;
use holzwave_server::http::routes_admin::{self, AdminServices};
use holzwave_server::http::routes_auth::{self, AuthServices};
use holzwave_server::http::routes_enrol::{self, Enrol, EnrolServices};
use holzwave_server::http::routes_unlock::{self, GateError, GateFuture, Material, ProvisionKey, UnlockGate, UnlockSecret};
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, Role};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::auth_env::{ADDR, AuthEnv, FIREFOX, FixedPage, Methods, PASSWORD, Tuning, WithCookies, set_cookie_value};
use crate::edge::{HOST, ORIGIN, Req, Resp, Signed, UID_RANGE, server_key};

/// The public address links are built on.
pub use crate::auth_env::PUBLIC_URL;

/// A sign-in older than the step-up time (300 seconds), in seconds.
pub const SIX_MINUTES: i64 = 360;

/// The gate of the environment: the fake of the sign-in environment behind a pass-through.
pub struct RecordingSecrets {
    inner: Arc<crate::auth_env::FakeGate>,
}

impl StoreView for RecordingSecrets {
    fn state(&self, account: AccountId) -> StoreState {
        self.inner.state(account)
    }
}

impl UnlockGate for RecordingSecrets {
    fn provision(&self, _account: AccountId, _keys: Vec<ProvisionKey>) -> GateFuture<'_, Result<(), GateError>> {
        Box::pin(async { Ok(()) })
    }

    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>> {
        self.inner.session_started(account, material)
    }

    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>> {
        self.inner.unlock_with(account, secret)
    }
}

/// The environment.
pub struct ApiEnv {
    pub auth: AuthEnv,
    pub secrets: Arc<RecordingSecrets>,
    pub accounts: Arc<Accounts>,
    pub enrol: Arc<Enrol>,
    router: Router,
}

impl Deref for ApiEnv {
    type Target = AuthEnv;

    fn deref(&self) -> &AuthEnv {
        &self.auth
    }
}

impl Default for ApiEnv {
    fn default() -> ApiEnv {
        ApiEnv::new()
    }
}

impl ApiEnv {
    /// An environment with the password and the passkey method on.
    pub fn new() -> ApiEnv {
        ApiEnv::build(Methods { passkey: true, password: true, oidc: false }, Tuning::default())
    }

    /// An environment with the given methods and tuning.
    pub fn build(methods: Methods, tuning: Tuning) -> ApiEnv {
        let auth = AuthEnv::build(methods, tuning);
        let clock: Arc<dyn Clock> = auth.clock.clone();
        let accounts = Arc::new(Accounts::new(auth.store.clone(), clock.clone(), auth.app.audit.clone(), UID_RANGE));
        let secrets = Arc::new(RecordingSecrets { inner: auth.gate.clone() });
        let services = Arc::new(AuthServices {
            app: auth.app.clone(),
            signin: auth.signin.clone(),
            passkeys: auth.passkeys.clone(),
            oidc: auth.oidc.clone(),
            gate: secrets.clone(),
            page: Arc::new(FixedPage),
        });
        let enrol = Enrol::new(EnrolServices {
            auth: services.clone(),
            accounts: accounts.clone(),
            argon: auth.argon.clone(),
            sealer: Arc::new(Sealer::new(&server_key())),
        });
        let admin = Arc::new(AdminServices::new(accounts.clone(), auth.signin.clone(), auth.app.origin.clone(), clock));
        let mut parts = routes_auth::parts(services.clone());
        parts.extend(routes_unlock::parts(services));
        parts.extend(routes_enrol::parts(&enrol));
        parts.extend(routes_admin::parts(admin));
        let router = router::build(auth.app.clone(), parts);
        ApiEnv { auth, secrets, accounts, enrol, router }
    }

    /// Sends a request to the router that holds every part.
    pub async fn send(&self, req: Req) -> Resp {
        let response = self.router.clone().oneshot(req.build()).await.expect("the router never fails");
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("the body can be read");
        Resp { status: parts.status, headers: parts.headers, body: bytes.to_vec() }
    }

    // ---- people with a session ----

    /// A session of `name` whose last sign-in was `age` seconds ago.
    fn signed_aged(&self, name: &str, age: i64) -> Signed {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        let created = self
            .sessions
            .create(account.id, SignInMethod::PasswordTotp, self.clock.unix() - age, "Firefox on Linux", Some(ADDR))
            .expect("a session");
        let csrf = self.app.csrf_token(&created.session);
        Signed { cookie: created.value.expose().to_owned(), session: created.session, account: account.id, csrf }
    }

    /// An administrator who signed in just now.
    pub fn admin_fresh(&self, name: &str) -> Signed {
        self.bare(name, Role::Admin);
        self.signed_aged(name, 0)
    }

    /// An administrator whose sign-in is six minutes old: outside the step-up window, inside the idle time.
    pub fn admin_stale(&self, name: &str) -> Signed {
        self.bare(name, Role::Admin);
        self.signed_aged(name, SIX_MINUTES)
    }

    /// A member who signed in just now.
    pub fn member(&self, name: &str) -> Signed {
        self.bare(name, Role::Member);
        self.signed_aged(name, 0)
    }

    /// Another session of an account that exists, signed in just now.
    pub fn another_session(&self, name: &str) -> Signed {
        self.signed_aged(name, 0)
    }

    // ---- requests ----

    /// A read with a session.
    pub fn get_as(&self, path: &str, who: &Signed) -> Req {
        self.get(path).signed(who)
    }

    /// A change of any method with a session and a JSON body.
    pub fn change_as(&self, method: Method, path: &str, body: &Value, who: &Signed) -> Req {
        Req::new(method, path)
            .header("host", HOST)
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .header("user-agent", FIREFOX)
            .json(body)
            .signed(who)
    }

    /// `POST` with a session.
    pub fn post_json(&self, path: &str, body: &Value, who: &Signed) -> Req {
        self.change_as(Method::POST, path, body, who)
    }

    // ---- redeeming ----

    /// A whole password enrolment through an invitation: `begin`, the code, `finish`. Answers the `finish` response and the
    /// authenticator secret.
    pub async fn redeem_with_password(&self, token: &str, username: &str) -> (Resp, Vec<u8>) {
        let begin = self
            .send(self.post(
                "/auth/invite/redeem",
                &json!({"step": "begin", "token": token, "username": username, "display_name": "Person", "method": "password", "password": PASSWORD}),
            ))
            .await;
        assert_eq!(begin.status, 200, "begin: {}", begin.text());
        let pending = set_cookie_value(&begin, "__Host-hw_pending").expect("a pending cookie");
        let text = begin.json()["secret"].as_str().expect("a secret").to_owned();
        let secret = totp_rs::Secret::try_from_base32(text).expect("base32").as_bytes().to_vec();
        let code = holzwave_server::auth::totp::code_at(&secret, self.clock.unix()).expect("a code").to_string();
        let finish = self
            .send(
                self.post("/auth/invite/redeem", &json!({"step": "finish", "code": code}))
                    .set_cookies(&[("__Host-hw_pending", &pending)]),
            )
            .await;
        (finish, secret)
    }

    // ---- what the run left behind ----

    /// The number of invitation rows, redeemed or not.
    pub fn invitation_rows(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.store.path()).expect("a second connection");
        conn.query_row("SELECT COUNT(*) FROM invites", [], |r| r.get(0)).expect("a count")
    }

    /// The `last_signin` of an account.
    pub fn last_signin(&self, id: AccountId) -> Option<i64> {
        self.store.read(|tx| tx.account_last_signin(id)).expect("read")
    }
}
