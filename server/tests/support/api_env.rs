//! The test environment of the account and administration routes: the sign-in environment, a recording gate that is also the
//! seam to the secret store, the accounts service, the enrolment routes (so that an invitation link can be redeemed), and a
//! router that holds the sign-in, unlock, enrolment, `/api/admin` and `/api/me` parts together.
//!
//! The test file declares `mod edge;`, `mod auth_env;` and `mod api_env;` at its root (all three with `#[path]`).
#![allow(dead_code)]

use std::ops::Deref;
use std::sync::{Arc, Mutex};

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
use holzwave_server::http::routes_me::{self, MeServices, SecretsAdmin, SecretsError};
use holzwave_server::http::routes_unlock::{self, GateError, GateFuture, Material, ProvisionKey, UnlockGate, UnlockSecret};
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, Role, UnlockKind};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::auth_env::webauthn_client::SoftPasskey;
use crate::auth_env::{
    ADDR, AuthEnv, FIREFOX, FixedPage, Methods, PASSWORD, Tuning, WithCookies, set_cookie_value,
};
use crate::edge::{HOST, ORIGIN, Req, Resp, Signed, UID_RANGE, server_key};

/// The public address links are built on.
pub use crate::auth_env::PUBLIC_URL;

/// A sign-in older than the step-up time (300 seconds), in seconds.
pub const SIX_MINUTES: i64 = 360;

/// The key a call of the seam handed over, as the fake saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySeen {
    pub kind: UnlockKind,
    pub credential_id: Option<Vec<u8>>,
    pub key: Vec<u8>,
}

impl KeySeen {
    fn of(key: &ProvisionKey) -> KeySeen {
        KeySeen { kind: key.kind(), credential_id: key.credential_id().map(<[u8]>::to_vec), key: key.key().to_vec() }
    }
}

/// One call of the seam to the secret store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotCall {
    Set { account: AccountId, key: KeySeen, via: Option<KeySeen> },
    Remove { account: AccountId, kind: UnlockKind, credential_id: Option<Vec<u8>> },
    Reset { account: AccountId },
}

/// What the fake answers next.
#[derive(Default)]
struct Plan {
    set: Option<SecretsError>,
    remove: Option<SecretsError>,
    reset: Option<SecretsError>,
    /// Calls of `set_slot` that answer ok before `set` applies.
    set_ok_first: usize,
}

/// The gate of the environment: the fake of the sign-in environment behind a pass-through, and the seam to the secret store
/// as a recorder with answers a test can choose.
pub struct RecordingSecrets {
    inner: Arc<crate::auth_env::FakeGate>,
    calls: Mutex<Vec<SlotCall>>,
    plan: Mutex<Plan>,
}

impl RecordingSecrets {
    /// Every call of the seam so far, in order.
    pub fn calls(&self) -> Vec<SlotCall> {
        self.calls.lock().expect("lock").clone()
    }

    /// The calls of `set_slot` so far.
    pub fn sets(&self) -> Vec<(AccountId, KeySeen, Option<KeySeen>)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                SlotCall::Set { account, key, via } => Some((account, key, via)),
                _ => None,
            })
            .collect()
    }

    /// The calls of `remove_slot` so far.
    pub fn removes(&self) -> Vec<(AccountId, UnlockKind, Option<Vec<u8>>)> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                SlotCall::Remove { account, kind, credential_id } => Some((account, kind, credential_id)),
                _ => None,
            })
            .collect()
    }

    /// The calls of `reset` so far.
    pub fn resets(&self) -> Vec<AccountId> {
        self.calls()
            .into_iter()
            .filter_map(|c| match c {
                SlotCall::Reset { account } => Some(account),
                _ => None,
            })
            .collect()
    }

    /// Makes `set_slot` fail with `error` (or succeed again with `None`).
    pub fn fail_set(&self, error: Option<SecretsError>) {
        self.plan.lock().expect("lock").set = error;
    }

    /// Makes `set_slot` answer ok `ok_first` times and then fail with `error`.
    pub fn fail_set_after(&self, ok_first: usize, error: SecretsError) {
        let mut plan = self.plan.lock().expect("lock");
        plan.set = Some(error);
        plan.set_ok_first = ok_first;
    }

    /// Makes `remove_slot` fail with `error` (or succeed again with `None`).
    pub fn fail_remove(&self, error: Option<SecretsError>) {
        self.plan.lock().expect("lock").remove = error;
    }

    /// Makes `reset` fail with `error` (or succeed again with `None`).
    pub fn fail_reset(&self, error: Option<SecretsError>) {
        self.plan.lock().expect("lock").reset = error;
    }
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

impl SecretsAdmin for RecordingSecrets {
    fn set_slot(
        &self,
        account: AccountId,
        key: ProvisionKey,
        via: Option<ProvisionKey>,
    ) -> GateFuture<'_, Result<(), SecretsError>> {
        self.calls.lock().expect("lock").push(SlotCall::Set {
            account,
            key: KeySeen::of(&key),
            via: via.as_ref().map(KeySeen::of),
        });
        let answer = {
            let mut plan = self.plan.lock().expect("lock");
            if plan.set_ok_first > 0 {
                plan.set_ok_first -= 1;
                None
            } else {
                plan.set
            }
        };
        Box::pin(async move { answer.map_or(Ok(()), Err) })
    }

    fn remove_slot(
        &self,
        account: AccountId,
        kind: UnlockKind,
        credential_id: Option<Vec<u8>>,
    ) -> GateFuture<'_, Result<(), SecretsError>> {
        self.calls.lock().expect("lock").push(SlotCall::Remove { account, kind, credential_id });
        let answer = self.plan.lock().expect("lock").remove;
        Box::pin(async move { answer.map_or(Ok(()), Err) })
    }

    fn reset(&self, account: AccountId) -> GateFuture<'_, Result<(), SecretsError>> {
        self.calls.lock().expect("lock").push(SlotCall::Reset { account });
        let answer = self.plan.lock().expect("lock").reset;
        Box::pin(async move { answer.map_or(Ok(()), Err) })
    }
}

/// The environment.
pub struct ApiEnv {
    pub auth: AuthEnv,
    pub secrets: Arc<RecordingSecrets>,
    pub accounts: Arc<Accounts>,
    pub enrol: Arc<Enrol>,
    pub me: Arc<MeServices>,
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
        ApiEnv::build_with(methods, tuning, routes_me::MAX_PENDING)
    }

    /// An environment whose account routes keep at most `max_pending` half-finished steps.
    pub fn build_with(methods: Methods, tuning: Tuning, max_pending: usize) -> ApiEnv {
        let auth = AuthEnv::build(methods, tuning);
        let clock: Arc<dyn Clock> = auth.clock.clone();
        let accounts = Arc::new(Accounts::new(auth.store.clone(), clock.clone(), auth.app.audit.clone(), UID_RANGE));
        let secrets = Arc::new(RecordingSecrets {
            inner: auth.gate.clone(),
            calls: Mutex::new(Vec::new()),
            plan: Mutex::new(Plan::default()),
        });
        let services = Arc::new(AuthServices {
            app: auth.app.clone(),
            signin: auth.signin.clone(),
            passkeys: auth.passkeys.clone(),
            oidc: auth.oidc.clone(),
            gate: secrets.clone(),
            page: Arc::new(FixedPage),
        });
        let sealer = Arc::new(Sealer::new(&server_key()));
        let enrol = Enrol::new(EnrolServices {
            auth: services.clone(),
            accounts: accounts.clone(),
            argon: auth.argon.clone(),
            sealer: sealer.clone(),
        });
        let admin = Arc::new(AdminServices::new(accounts.clone(), auth.signin.clone(), auth.app.origin.clone(), clock));
        let me = Arc::new(
            MeServices::new(
                auth.app.clone(),
                auth.signin.clone(),
                auth.passkeys.clone(),
                auth.argon.clone(),
                sealer,
                secrets.clone(),
            )
            .with_max_pending(max_pending),
        );
        let mut parts = routes_auth::parts(services.clone());
        parts.extend(routes_unlock::parts(services));
        parts.extend(routes_enrol::parts(&enrol));
        parts.extend(routes_admin::parts(admin));
        parts.extend(routes_me::parts(me.clone()));
        let router = router::build(auth.app.clone(), parts);
        ApiEnv { auth, secrets, accounts, enrol, me, router }
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
    pub fn signed_aged(&self, name: &str, age: i64) -> Signed {
        self.signed_with(name, age, SignInMethod::PasswordTotp)
    }

    /// A session of `name` made with `method`, whose last sign-in was `age` seconds ago.
    pub fn signed_with(&self, name: &str, age: i64, method: SignInMethod) -> Signed {
        let account = self.store.account_by_username(name).expect("read").expect("the account exists");
        let created = self
            .sessions
            .create(account.id, method, self.clock.unix() - age, "Firefox on Linux", Some(ADDR))
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

    /// A member with a password and an authenticator who signed in just now.
    pub async fn member_with_password(&self, name: &str) -> Signed {
        self.person(name).await;
        self.signed_aged(name, 0)
    }

    /// A member with a password and an authenticator whose sign-in is six minutes old.
    pub async fn stale_member_with_password(&self, name: &str) -> Signed {
        self.person(name).await;
        self.signed_aged(name, SIX_MINUTES)
    }

    /// A member whose only way in is one passkey, signed in just now with it.
    pub fn member_with_passkey(&self, name: &str) -> (Signed, SoftPasskey) {
        let (_, key) = self.passkey_person(name);
        (self.signed_with(name, 0, SignInMethod::Passkey), key)
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

    /// `PUT` with a session.
    pub fn put_json(&self, path: &str, body: &Value, who: &Signed) -> Req {
        self.change_as(Method::PUT, path, body, who)
    }

    /// `DELETE` with a session (the page sends no body, so none is sent).
    pub fn delete_as(&self, path: &str, who: &Signed) -> Req {
        Req::new(Method::DELETE, path)
            .header("host", HOST)
            .header("origin", ORIGIN)
            .header("content-type", "application/json")
            .header("user-agent", FIREFOX)
            .signed(who)
    }

    /// `GET /api/me`.
    pub async fn me_of(&self, who: &Signed) -> Resp {
        self.send(self.get_as("/api/me", who)).await
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
        self.count_rows("SELECT COUNT(*) FROM invites")
    }

    /// The result of a counting query on a second connection.
    pub fn count_rows(&self, sql: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.store.path()).expect("a second connection");
        conn.query_row(sql, [], |r| r.get(0)).expect("a count")
    }

    /// The `last_signin` of an account.
    pub fn last_signin(&self, id: AccountId) -> Option<i64> {
        self.store.read(|tx| tx.account_last_signin(id)).expect("read")
    }

    /// The unlock methods of an account as kind and credential id, in the order they were added.
    pub fn unlock_kinds(&self, id: AccountId) -> Vec<(UnlockKind, Option<Vec<u8>>)> {
        self.store
            .read(|tx| tx.unlock_list(id))
            .expect("list")
            .into_iter()
            .map(|m| (m.kind, m.credential_id))
            .collect()
    }

    /// Makes every matching write to `table` fail, from a second connection, the way a full disk or a broken file would.
    /// `name` names the trigger so that several can exist.
    pub fn break_writes(&self, name: &str, event: &str, table: &str, when: &str) {
        let conn = rusqlite::Connection::open(self.store.path()).expect("a second connection");
        conn.execute_batch(&format!(
            "CREATE TRIGGER {name} BEFORE {event} ON {table} {when} BEGIN SELECT RAISE(ABORT, 'broken'); END;"
        ))
        .expect("the trigger is installed");
    }

    /// Takes a trigger made by [`ApiEnv::break_writes`] away.
    pub fn repair(&self, name: &str) {
        let conn = rusqlite::Connection::open(self.store.path()).expect("a second connection");
        conn.execute_batch(&format!("DROP TRIGGER {name};")).expect("the trigger is dropped");
    }

    /// Every table that holds a credential or a secret of an account, as a text, to see that a failed change left them as
    /// they were.
    pub fn credential_dump(&self, id: AccountId) -> String {
        let conn = rusqlite::Connection::open(self.store.path()).expect("a second connection");
        let mut out = String::new();
        for sql in [
            "SELECT hex(verifier) || ':' || hex(salt) || ':' || params || ':' || changed_at FROM password_credentials WHERE account_id = ?1",
            "SELECT hex(sealed_secret) || ':' || last_step FROM totp WHERE account_id = ?1",
            "SELECT hex(credential_id) || ':' || passkey_json || ':' || hex(coalesce(prf_salt, x'')) FROM passkeys WHERE account_id = ?1 ORDER BY id",
            "SELECT kind || ':' || hex(salt) || ':' || params || ':' || hex(coalesce(credential_id, x'')) FROM unlock_methods WHERE account_id = ?1 ORDER BY rowid",
        ] {
            let mut stmt = conn.prepare(sql).expect("prepare");
            let rows: Vec<String> = stmt
                .query_map([id.get()], |r| r.get::<_, String>(0))
                .expect("query")
                .collect::<rusqlite::Result<_>>()
                .expect("rows");
            out.push_str(&rows.join("\n"));
            out.push('\n');
        }
        out
    }
}
