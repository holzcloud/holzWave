//! The routes of the signed-in person's own account under `/api/me`: what the account holds, the password, the second factor,
//! passkeys, the passphrase and the recovery code of the secret store, and the sessions.
//!
//! - **One mount, one person.** Every route is in one [`Access::SessionFresh`] part, so the router has answered 401 for no
//!   session, checked the anti-forgery token and refused a change after an old sign-in (403 with `step_up`) before a handler
//!   runs. The account a handler changes is the account of the session; no path and no body names another.
//! - **The seam to the secret store.** A change of a credential that wraps the data key of the secret store goes through a
//!   [`SecretsAdmin`] first and the database second, and a database that fails puts the seam back. A refusal of the seam is
//!   told as what it is (423 `locked`, 503 `busy`, 409 on `method`), and nothing changes.
//! - **Nothing secret is written down.** A password, a code, a passphrase and a recovery code are in no log line, no audit line
//!   and no answer but the one that must carry them (the authenticator secret and the recovery code, once each).
//! - **Half-finished steps are bounded.** An authenticator or a passkey enrolment waits for its second step in memory, one per
//!   person and kind, for five minutes, at most [`MAX_PENDING`] together; it is wiped when it goes.
//! - **A session is named by a handle.** The id of a session in an answer is the first 12 bytes of its stored hash, which is not
//!   the cookie value and opens nothing.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::audit::{Action, Actor, DetailKey, DetailValue, Event, Outcome};
use crate::auth::passkey::{ChallengeToken, PasskeyError, Passkeys, RegistrationSubject};
use crate::auth::password::{ArgonGate, PasswordError, check_policy, params_text, prepare_password, store_prepared};
use crate::auth::session::handle_of;
use crate::auth::signin::SignIn;
use crate::auth::totp;
use crate::clock::{from_unix, rfc3339};
use crate::http::app::App;
use crate::http::router::{Access, ApiError, ApiJson, Authed, Part};
use crate::http::routes_unlock::{GateFuture, ProvisionKey};
use crate::log::{self, Level};
use crate::sealed::Sealer;
use crate::store::{AccountId, Store, StoreError, UnlockKind};
use crate::unlock::{SALT_BYTES, generate_recovery_code, kek_from_prf, kek_from_recovery, random};

use super::cookies;
use super::routes_auth::{GATE_TIMEOUT, Object, Refusal, answer, blocking};

/// The most half-finished steps (authenticator and passkey enrolments) that are open at once.
pub const MAX_PENDING: usize = 1_024;

/// How long a half-finished step waits for its second step, in seconds.
const PENDING_SECS: i64 = 300;

/// How many wrong codes an authenticator enrolment allows. The last one ends it.
const CODE_ATTEMPTS: u8 = 3;

/// The parameters text of an unlock row whose key comes from HKDF and not from a password hash.
const HKDF_PARAMS: &str = "hkdf-sha256";

/// The most characters of a passkey name that are accepted.
const MAX_NAME_CHARS: usize = crate::auth::passkey::MAX_NAME_CHARS;

/// The most digits of a row id in a path.
const MAX_ID_DIGITS: usize = 18;

// ---------------------------------------------------------------------------------------------------------------------
// The seam to the secret store
// ---------------------------------------------------------------------------------------------------------------------

/// Why the secret store did not do what it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretsError {
    /// The store is locked and the key given could not open it.
    Locked,
    /// The worker side could not be reached or failed.
    Unavailable,
    /// The worker side refused the change.
    Refused,
}

/// The seam between the account routes and the secret store of the worker side. A key is handed over once and wiped when the
/// call is done with it. The implementation, not the route, writes the `secrets.*` audit lines.
pub trait SecretsAdmin: Send + Sync {
    /// Sets the slot of `key`'s kind (and credential, for a PRF key) so that `key` opens the data key. `via` is a key that
    /// opens the store now, for a store that is locked or for a slot that replaces the one `via` came from; `None` asks the
    /// worker side to use the store as it is open.
    fn set_slot(
        &self,
        account: AccountId,
        key: ProvisionKey,
        via: Option<ProvisionKey>,
    ) -> GateFuture<'_, Result<(), SecretsError>>;

    /// Removes the slot of `kind` (for a PRF key, of the credential).
    fn remove_slot(
        &self,
        account: AccountId,
        kind: UnlockKind,
        credential_id: Option<Vec<u8>>,
    ) -> GateFuture<'_, Result<(), SecretsError>>;

    /// Removes the whole secret store of the account.
    fn reset(&self, account: AccountId) -> GateFuture<'_, Result<(), SecretsError>>;
}

/// What the seam's refusal looks like to the browser.
fn seam_refusal(error: SecretsError) -> Refusal {
    Refusal(match error {
        SecretsError::Locked => ApiError::Locked,
        SecretsError::Unavailable => ApiError::Busy,
        SecretsError::Refused => ApiError::Conflict { field: Some("method") },
    })
}

/// Waits for the seam, but not longer than [`GATE_TIMEOUT`]: a seam that does not answer is unavailable.
pub(super) async fn seam<T>(call: GateFuture<'_, Result<T, SecretsError>>) -> Result<T, SecretsError> {
    tokio::time::timeout(GATE_TIMEOUT, call).await.unwrap_or(Err(SecretsError::Unavailable))
}

// ---------------------------------------------------------------------------------------------------------------------
// The half-finished steps
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StepKind {
    Totp,
    Passkey,
}

/// What a half-finished step holds until its second step. Wiped when dropped.
enum Held {
    /// The new authenticator secret.
    Totp(Zeroizing<[u8; totp::SECRET_BYTES]>),
    /// The challenge of a passkey registration and the name the person chose.
    Passkey { challenge: ChallengeToken, name: String },
}

struct Step {
    held: Held,
    created: i64,
    attempts_left: u8,
}

#[derive(Default)]
struct Steps {
    map: HashMap<(i64, StepKind), Step>,
}

impl Steps {
    fn sweep(&mut self, now: i64) {
        self.map.retain(|_, step| now <= step.created.saturating_add(PENDING_SECS));
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The services
// ---------------------------------------------------------------------------------------------------------------------

/// The services the account routes call.
pub struct MeServices {
    app: Arc<App>,
    signin: Arc<SignIn>,
    passkeys: Arc<Passkeys>,
    argon: ArgonGate,
    sealer: Arc<Sealer>,
    secrets: Arc<dyn SecretsAdmin>,
    max_pending: usize,
    steps: Mutex<Steps>,
}

impl MeServices {
    /// The services over the application state, the sign-in (for the check of the current password), the passkeys, the
    /// derivation gate, the sealer of the authenticator secrets and the seam to the secret store.
    pub fn new(
        app: Arc<App>,
        signin: Arc<SignIn>,
        passkeys: Arc<Passkeys>,
        argon: ArgonGate,
        sealer: Arc<Sealer>,
        secrets: Arc<dyn SecretsAdmin>,
    ) -> MeServices {
        MeServices { app, signin, passkeys, argon, sealer, secrets, max_pending: MAX_PENDING, steps: Mutex::new(Steps::default()) }
    }

    /// The same services with room for fewer half-finished steps than [`MAX_PENDING`] (never more). For tests that fill the
    /// room.
    pub fn with_max_pending(mut self, most: usize) -> MeServices {
        self.max_pending = most.clamp(1, MAX_PENDING);
        self
    }

    /// How many half-finished steps are open now.
    pub fn pending_count(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> MutexGuard<'_, Steps> {
        self.steps.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> i64 {
        self.app.clock.unix()
    }

    /// Keeps a half-finished step, replacing the person's earlier step of the same kind. A step beyond the bound is 503 once
    /// the steps that ran out have been swept.
    fn keep(&self, account: AccountId, kind: StepKind, held: Held, now: i64) -> Result<(), ApiError> {
        let mut steps = self.lock();
        let key = (account.get(), kind);
        if !steps.map.contains_key(&key) && steps.map.len() >= self.max_pending {
            steps.sweep(now);
            if steps.map.len() >= self.max_pending {
                return Err(ApiError::Busy);
            }
        }
        steps.map.insert(key, Step { held, created: now, attempts_left: CODE_ATTEMPTS });
        Ok(())
    }

    /// Takes the person's step of a kind out, if it is there and has not run out.
    fn take(&self, account: AccountId, kind: StepKind, now: i64) -> Option<Step> {
        let step = self.lock().map.remove(&(account.get(), kind))?;
        (now <= step.created.saturating_add(PENDING_SECS)).then_some(step)
    }

    /// Puts a step back after a wrong code (one attempt fewer, none left ends it) or after a failure that was not the person's
    /// (`spend` false). A step the person began again in the meantime is kept.
    fn put_back(&self, account: AccountId, kind: StepKind, mut step: Step, spend: bool) {
        if spend {
            step.attempts_left = step.attempts_left.saturating_sub(1);
        }
        if step.attempts_left > 0 {
            self.lock().map.entry((account.get(), kind)).or_insert(step);
        }
    }

    fn record(&self, event: &Event, what: &'static str) {
        if self.app.audit.record(event).is_err() {
            log::event(Level::Error, what, &[]);
        }
    }
}

type Shared = Arc<MeServices>;

// ---------------------------------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------------------------------

fn bad(field: &'static str) -> Refusal {
    Refusal(ApiError::BadRequest { field: Some(field) })
}

fn busy() -> Refusal {
    Refusal(ApiError::Busy)
}

fn time_text(unix: i64) -> String {
    rfc3339(from_unix(unix))
}

/// A member that must be a string.
fn text_member(value: Option<Value>, field: &'static str) -> Result<Zeroizing<String>, Refusal> {
    match value {
        Some(Value::String(text)) => Ok(Zeroizing::new(text)),
        _ => Err(bad(field)),
    }
}

/// A 204 with `no-store`.
fn no_content(cookies: Vec<HeaderValue>) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// The text of a row id in a path: plain digits, at most [`MAX_ID_DIGITS`] of them. Anything else names no row.
fn row_id(path: Result<Path<String>, PathRejection>) -> Option<i64> {
    let Path(text) = path.ok()?;
    if text.is_empty() || text.len() > MAX_ID_DIGITS || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A body with no member: `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn store_error(error: PasswordError) -> StoreError {
    match error {
        PasswordError::Store(e) => e,
        _ => StoreError::Invalid("the password credential could not be stored"),
    }
}

/// Whether the secret store of the account is locked, as the request saw it.
fn require_open(authed: &Authed) -> Result<(), Refusal> {
    if authed.locked { Err(Refusal(ApiError::Locked)) } else { Ok(()) }
}

/// Whether the account has a password; `404` when it has not.
async fn require_password(services: &Shared, account: AccountId) -> Result<(), Refusal> {
    let store = services.app.store.clone();
    let has = blocking(move || store.read(|tx| tx.password_get(account)).map(|found| found.is_some()))
        .await?
        .map_err(|_| busy())?;
    if has { Ok(()) } else { Err(Refusal(ApiError::NotFound)) }
}

// ---------------------------------------------------------------------------------------------------------------------
// GET /api/me
// ---------------------------------------------------------------------------------------------------------------------

/// `GET /api/me`: what the account holds, with no secret in it.
async fn me_route(State(services): State<Shared>, authed: Authed) -> Result<Response, Refusal> {
    let account = authed.account.clone();
    let current = handle_of(&authed.session);
    let locked = authed.locked;
    let worker = services.clone();
    let body = blocking(move || -> Result<Value, ApiError> {
        let app = &worker.app;
        let now = app.clock.unix();
        let (password, totp_set, identities, unlock) = app
            .store
            .read(|tx| {
                Ok((
                    tx.password_get(account.id)?.is_some(),
                    tx.totp_get(account.id)?.is_some(),
                    tx.oidc_list(account.id)?,
                    tx.unlock_list(account.id)?,
                ))
            })
            .map_err(|_| ApiError::Busy)?;
        let passkeys = worker.passkeys.list(account.id).map_err(|_| ApiError::Busy)?;
        let sessions = app.session_manager.list_live(account.id, now).map_err(|_| ApiError::Busy)?;
        let oidc = identities
            .iter()
            .find_map(|identity| app.auth.oidc.iter().find(|provider| provider.issuer == identity.issuer))
            .map(|provider| json!({ "id": provider.id, "name": provider.name }));
        let passkeys: Vec<Value> = passkeys
            .iter()
            .map(|key| {
                json!({
                    "id": key.id,
                    "name": key.name,
                    "created_at": time_text(key.created_at),
                    "last_used": key.last_used.map(time_text),
                })
            })
            .collect();
        let sessions: Vec<Value> = sessions
            .iter()
            .map(|session| {
                let handle = handle_of(session);
                json!({
                    "current": handle == current,
                    "id": handle,
                    "device": session.device,
                    "addr": session.addr,
                    "created_at": time_text(session.created_at),
                    "last_seen": time_text(session.last_seen),
                })
            })
            .collect();
        Ok(json!({
            "account": {
                "id": account.id.get(),
                "username": account.username,
                "display_name": account.display_name,
                "role": account.role.as_str(),
            },
            "oidc": oidc,
            "password": password,
            "totp": totp_set,
            "passkeys": passkeys,
            "secrets": {
                "state": if locked { "locked" } else { "unlocked" },
                "passphrase": unlock.iter().any(|m| m.kind == UnlockKind::Passphrase),
                "recovery": unlock.iter().any(|m| m.kind == UnlockKind::Recovery),
            },
            "sessions": sessions,
        }))
    })
    .await?
    .map_err(Refusal)?;
    Ok(answer(StatusCode::OK, &body, Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/password
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    #[serde(default)]
    current: Option<Value>,
    #[serde(default)]
    new: Option<Value>,
}

/// `POST /api/me/password`: in order, the current password is checked (the sign-in's own gate and lock), the new one is held to
/// the policy and derived, the seam sets the slot of the new password opened by the old key, and the database takes the new
/// credential and its unlock row in one transaction. A database that fails sets the seam back with the keys exchanged.
async fn password_route(
    State(services): State<Shared>,
    authed: Authed,
    ApiJson(Object(body)): ApiJson<Object<PasswordBody>>,
) -> Result<Response, Refusal> {
    let current = text_member(body.current, "current")?;
    let new = text_member(body.new, "new")?;
    let account = authed.account.id;
    let addr = authed.client;
    require_password(&services, account).await?;

    let old = services.signin.verify_current(account, &authed.account.username, &current, addr).await?;
    drop(current);
    let prepared = match prepare_password(&services.argon, &authed.account.username, &new).await {
        Ok(prepared) => Arc::new(prepared),
        Err(PasswordError::Policy(_)) => return Err(bad("new")),
        Err(_) => return Err(busy()),
    };
    drop(new);

    let key = ProvisionKey::new(UnlockKind::Password, None, prepared.kek());
    let via = ProvisionKey::new(UnlockKind::Password, None, old.kek());
    seam(services.secrets.set_slot(account, key, Some(via))).await.map_err(seam_refusal)?;

    let now = services.now();
    let store = services.app.store.clone();
    let kept = prepared.clone();
    let stored = blocking(move || {
        store.write(|tx| {
            store_prepared(tx, account, &kept, now).map_err(store_error)?;
            tx.unlock_replace(account, UnlockKind::Password, kept.salt(), kept.params(), None)?;
            Ok(())
        })
    })
    .await?;
    if stored.is_err() {
        // The slot is put back to what the database still holds: the old key, opened by the new one.
        let back = ProvisionKey::new(UnlockKind::Password, None, old.kek());
        let via = ProvisionKey::new(UnlockKind::Password, None, prepared.kek());
        let _ = seam(services.secrets.set_slot(account, back, Some(via))).await;
        return Err(busy());
    }
    services.record(
        &Event::new(Action::AuthPasswordChange, Outcome::Ok, Actor::Account(account), Some(addr)).target(account),
        "me.audit_failed",
    );
    Ok(no_content(Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/totp
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TotpBody {
    #[serde(default)]
    step: Option<Value>,
    #[serde(default)]
    code: Option<Value>,
}

/// The issuer an authenticator app shows: the host of the public address, or the product name when the host cannot be a label.
fn issuer(host: &str) -> &str {
    if host.is_empty() || host.contains(':') { "holzWave" } else { host }
}

/// `POST /api/me/totp`: `begin` shows a new secret and keeps it; `finish` with a code from the app that holds it replaces the
/// authenticator. The old authenticator stays until the finish succeeds, and the confirmed step is spent.
async fn totp_route(
    State(services): State<Shared>,
    authed: Authed,
    ApiJson(Object(body)): ApiJson<Object<TotpBody>>,
) -> Result<Response, Refusal> {
    let account = authed.account.id;
    let now = services.now();
    let step = match body.step {
        Some(Value::String(step)) if step == "begin" || step == "finish" => step,
        _ => return Err(bad("step")),
    };
    if step == "begin" {
        if body.code.is_some() {
            return Err(Refusal(ApiError::BadRequest { field: None }));
        }
        require_password(&services, account).await?;
        let secret = totp::generate_secret().map_err(|_| busy())?;
        let uri = totp::provisioning_uri(issuer(services.app.origin.host()), &authed.account.username, &secret[..])
            .map_err(|_| busy())?;
        let base32 = Zeroizing::new(totp_rs::Secret::from(&secret[..]).to_base32());
        let shown = json!({ "secret": base32.as_str(), "otpauth": uri.as_str() });
        services.keep(account, StepKind::Totp, Held::Totp(secret), now)?;
        return Ok(answer(StatusCode::OK, &shown, Vec::new()));
    }

    let code = text_member(body.code, "code")?;
    require_password(&services, account).await?;
    let Some(taken) = services.take(account, StepKind::Totp, now) else {
        return Err(Refusal(ApiError::Invalid));
    };
    let Held::Totp(secret) = &taken.held else {
        return Err(Refusal(ApiError::Invalid));
    };
    let confirmed = match totp::check_candidate(&secret[..], &code, now) {
        Ok(confirmed) => confirmed,
        Err(_) => {
            services.put_back(account, StepKind::Totp, taken, true);
            return Err(Refusal(ApiError::Invalid));
        }
    };
    let store = services.app.store.clone();
    let sealer = services.sealer.clone();
    let secret_copy = Zeroizing::new(match &taken.held {
        Held::Totp(secret) => **secret,
        Held::Passkey { .. } => [0u8; totp::SECRET_BYTES],
    });
    let stored = blocking(move || {
        store.write(|tx| {
            totp::store_secret(tx, &sealer, account, &secret_copy[..], confirmed)
                .map_err(|_| StoreError::Invalid("the authenticator could not be stored"))
        })
    })
    .await?;
    if stored.is_err() {
        // The failure was not the person's: the step stays as it was, and they may try the same code again.
        services.put_back(account, StepKind::Totp, taken, false);
        return Err(busy());
    }
    services.record(
        &Event::new(Action::AuthTotpEnrol, Outcome::Ok, Actor::Account(account), Some(authed.client)).target(account),
        "me.audit_failed",
    );
    Ok(no_content(Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/passkeys and DELETE /api/me/passkeys/{id}
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasskeyBody {
    #[serde(default)]
    step: Option<Value>,
    #[serde(default)]
    name: Option<Value>,
    #[serde(default)]
    credential: Option<Value>,
    #[serde(default)]
    prf: Option<Value>,
}

/// The refusal for an error of the passkey service.
fn passkey_refusal(error: PasskeyError) -> Refusal {
    Refusal(match error {
        PasskeyError::Invalid | PasskeyError::NotFound => ApiError::Invalid,
        PasskeyError::Conflict | PasskeyError::TooMany => ApiError::Conflict { field: Some("method") },
        PasskeyError::Internal => ApiError::Busy,
    })
}

/// The name a person gave a passkey: absent, or 1 to 64 characters without a control character.
fn passkey_name(value: Option<Value>) -> Result<String, Refusal> {
    match value {
        None => Ok(String::new()),
        Some(Value::String(name)) => {
            if name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
                Err(bad("name"))
            } else {
                Ok(name)
            }
        }
        Some(_) => Err(bad("name")),
    }
}

/// `POST /api/me/passkeys`: `begin` answers the creation options and keeps the challenge; `finish` verifies the response,
/// stores the passkey and, when the browser sent PRF output, sets a slot for it. The slot is the seam's best effort: a passkey
/// that is registered stays registered whatever the seam says, and has no unlock row when the slot was not set.
async fn passkeys_route(
    State(services): State<Shared>,
    authed: Authed,
    ApiJson(Object(body)): ApiJson<Object<PasskeyBody>>,
) -> Result<Response, Refusal> {
    if !services.app.auth.passkey {
        return Err(Refusal(ApiError::NotFound));
    }
    let account = authed.account.id;
    let client = authed.client;
    let now = services.now();
    let step = match body.step {
        Some(Value::String(step)) if step == "begin" || step == "finish" => step,
        _ => return Err(bad("step")),
    };

    if step == "begin" {
        if body.credential.is_some() || body.prf.is_some() {
            return Err(Refusal(ApiError::BadRequest { field: None }));
        }
        let name = passkey_name(body.name)?;
        let subject = RegistrationSubject {
            handle: authed.account.handle,
            user_name: authed.account.username.clone(),
            display_name: authed.account.display_name.clone(),
        };
        let passkeys = services.passkeys.clone();
        let (options, challenge) = blocking(move || passkeys.start_registration(&subject, client))
            .await?
            .map_err(passkey_refusal_for_begin)?;
        let options: Value = serde_json::from_str(&options).map_err(|_| busy())?;
        services.keep(account, StepKind::Passkey, Held::Passkey { challenge, name }, now)?;
        return Ok(answer(StatusCode::OK, &json!({ "options": options }), Vec::new()));
    }

    if body.name.is_some() {
        return Err(Refusal(ApiError::BadRequest { field: None }));
    }
    let Some(credential) = body.credential.filter(Value::is_object) else { return Err(bad("credential")) };
    let prf = match body.prf {
        None => None,
        Some(Value::String(text)) => Some(Zeroizing::new(text)),
        Some(_) => return Err(bad("prf")),
    };
    let Some(taken) = services.take(account, StepKind::Passkey, now) else {
        return Err(Refusal(ApiError::Invalid));
    };
    let Held::Passkey { challenge, name } = taken.held else {
        return Err(Refusal(ApiError::Invalid));
    };
    let passkeys = services.passkeys.clone();
    let credential = Zeroizing::new(credential.to_string());
    let registered = blocking(move || {
        passkeys.finish_registration(challenge.expose(), &credential, prf.as_deref().map(String::as_str), &name, client)
    })
    .await?
    .map_err(passkey_refusal)?;

    if let Some(output) = &registered.prf
        && let Ok(kek) = kek_from_prf(output.expose(), &registered.prf_salt)
    {
        let key = ProvisionKey::new(UnlockKind::Prf, Some(registered.credential_id.clone()), &kek);
        if seam(services.secrets.set_slot(account, key, None)).await.is_ok() {
            let store = services.app.store.clone();
            let (salt, credential_id) = (registered.prf_salt.clone(), registered.credential_id.clone());
            let added = blocking(move || {
                store.write(|tx| tx.unlock_add(account, UnlockKind::Prf, &salt, HKDF_PARAMS, Some(&credential_id)))
            })
            .await?;
            if added.is_err() {
                // A slot with no row cannot be used at sign-in: take it back.
                let _ = seam(services.secrets.remove_slot(account, UnlockKind::Prf, Some(registered.credential_id.clone()))).await;
            }
        }
    }
    Ok(answer(StatusCode::OK, &json!({ "id": registered.id }), Vec::new()))
}

/// The refusal for an error of the start of a registration: a person with all their keys is told so on `method`.
fn passkey_refusal_for_begin(error: PasskeyError) -> Refusal {
    Refusal(match error {
        PasskeyError::TooMany | PasskeyError::Conflict => ApiError::Conflict { field: Some("method") },
        _ => ApiError::Busy,
    })
}

/// `DELETE /api/me/passkeys/{id}`: removes one passkey of the account, with the unlock row of its credential and its slot. The
/// last way to sign in cannot go (409 on `method`); an id that is not the account's, or is not a plain number, is 404.
async fn passkey_remove_route(
    State(services): State<Shared>,
    authed: Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let Some(id) = row_id(path) else { return Err(Refusal(ApiError::NotFound)) };
    let account = authed.account.id;
    let client = authed.client;
    let worker = services.clone();
    let outcome = blocking(move || -> Result<Vec<u8>, ApiError> {
        // The credential id is looked up before the row goes: the unlock row is found by it.
        let rows = worker.app.store.read(|tx| tx.passkey_list(account)).map_err(|_| ApiError::Busy)?;
        let Some(row) = rows.into_iter().find(|row| row.id == id) else { return Err(ApiError::NotFound) };
        match worker.passkeys.remove(account, id, Some(client)) {
            Ok(()) => Ok(row.credential_id),
            Err(PasskeyError::NotFound) => Err(ApiError::NotFound),
            Err(PasskeyError::Conflict) => Err(ApiError::Conflict { field: Some("method") }),
            Err(_) => Err(ApiError::Busy),
        }
    })
    .await?
    .map_err(Refusal)?;

    let store = services.app.store.clone();
    let credential_id = outcome.clone();
    let removed = blocking(move || store.write(|tx| tx.unlock_remove_credential(account, &credential_id))).await?;
    match removed {
        Ok(rows) if rows > 0 => {
            // The slot is the seam's to remove; a seam that cannot leaves a slot nobody can reach any more.
            let _ = seam(services.secrets.remove_slot(account, UnlockKind::Prf, Some(outcome))).await;
        }
        Ok(_) => {}
        Err(_) => log::event(Level::Error, "me.unlock_row_not_removed", &[]),
    }
    Ok(no_content(Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// PUT /api/me/passphrase and POST /api/me/recovery
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PassphraseBody {
    #[serde(default)]
    passphrase: Option<Value>,
}

/// `PUT /api/me/passphrase`: sets the passphrase that unlocks the secret store. It needs the store open (423 otherwise); the
/// text is held to the policy of passwords (400 on `passphrase`), gets its own salt and one derivation through the gate, and
/// the seam sets its slot before the database takes the row. A row that cannot be written takes the slot back.
async fn passphrase_route(
    State(services): State<Shared>,
    authed: Authed,
    ApiJson(Object(body)): ApiJson<Object<PassphraseBody>>,
) -> Result<Response, Refusal> {
    let text = text_member(body.passphrase, "passphrase")?;
    check_policy(&authed.account.username, &text).map_err(|_| bad("passphrase"))?;
    require_open(&authed)?;
    let account = authed.account.id;
    let salt = random::<SALT_BYTES>().map_err(|_| busy())?;
    let kdf = services.argon.kdf();
    let kek = match services.argon.derive_passphrase(&text, salt, kdf).await {
        Ok(kek) => kek,
        Err(_) => return Err(busy()),
    };
    drop(text);

    let key = ProvisionKey::new(UnlockKind::Passphrase, None, &kek);
    seam(services.secrets.set_slot(account, key, None)).await.map_err(seam_refusal)?;
    let store = services.app.store.clone();
    let params = params_text(&kdf);
    let stored = blocking(move || store.write(|tx| tx.unlock_replace(account, UnlockKind::Passphrase, &salt, &params, None))).await?;
    if stored.is_err() {
        let _ = seam(services.secrets.remove_slot(account, UnlockKind::Passphrase, None)).await;
        return Err(busy());
    }
    services.record(
        &Event::new(Action::AuthPassphraseSet, Outcome::Ok, Actor::Account(account), Some(authed.client)).target(account),
        "me.audit_failed",
    );
    Ok(no_content(Vec::new()))
}

/// `POST /api/me/recovery`: replaces the recovery code of the secret store. The new code is in this answer and nowhere else, and
/// only after the seam has set its slot and the database has taken its row. It needs the store open (423 otherwise).
async fn recovery_route(
    State(services): State<Shared>,
    authed: Authed,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    require_open(&authed)?;
    let account = authed.account.id;
    let recovery = generate_recovery_code().map_err(|_| busy())?;
    let salt = random::<SALT_BYTES>().map_err(|_| busy())?;
    let kek = kek_from_recovery(&recovery, &salt).map_err(|_| busy())?;
    let text = recovery.display();

    let key = ProvisionKey::new(UnlockKind::Recovery, None, &kek);
    seam(services.secrets.set_slot(account, key, None)).await.map_err(seam_refusal)?;
    let store = services.app.store.clone();
    let stored =
        blocking(move || store.write(|tx| tx.unlock_replace(account, UnlockKind::Recovery, &salt, HKDF_PARAMS, None))).await?;
    if stored.is_err() {
        let _ = seam(services.secrets.remove_slot(account, UnlockKind::Recovery, None)).await;
        return Err(busy());
    }
    services.record(
        &Event::new(Action::RecoveryReplace, Outcome::Ok, Actor::Account(account), Some(authed.client)).target(account),
        "me.audit_failed",
    );
    Ok(answer(StatusCode::OK, &json!({ "recovery_code": text.as_str() }), Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// DELETE /api/me/sessions/{handle} and DELETE /api/me/sessions
// ---------------------------------------------------------------------------------------------------------------------

fn signout_event(account: AccountId, addr: IpAddr) -> Event {
    Event::new(Action::Signout, Outcome::Ok, Actor::Account(account), Some(addr))
}

/// `DELETE /api/me/sessions/{handle}`: ends one session of the account. The handle names a row of this account only; any
/// other text, and a handle of another account's session, is 404. Ending this very session deletes the cookie too.
async fn session_end_route(
    State(services): State<Shared>,
    authed: Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let Ok(Path(handle)) = path else { return Err(Refusal(ApiError::NotFound)) };
    let account = authed.account.id;
    let addr = authed.client;
    let own = handle_of(&authed.session) == handle;
    let worker = services.clone();
    let ended = blocking(move || worker.app.session_manager.end_by_handle(account, &handle)).await?.map_err(|_| busy())?;
    if !ended {
        return Err(Refusal(ApiError::NotFound));
    }
    services.record(&signout_event(account, addr), "signout.audit_failed");
    Ok(no_content(if own { vec![cookies::delete_session()] } else { Vec::new() }))
}

/// `DELETE /api/me/sessions`: ends every session of the account, this one included, and deletes the cookie. One `signout`
/// line says how many.
async fn sessions_end_all_route(State(services): State<Shared>, authed: Authed) -> Result<Response, Refusal> {
    let account = authed.account.id;
    let addr = authed.client;
    let worker = services.clone();
    let ended = blocking(move || worker.app.session_manager.end_all(account)).await?.map_err(|_| busy())?;
    let count = i64::try_from(ended).unwrap_or(i64::MAX);
    services.record(&signout_event(account, addr).detail(DetailKey::Count, DetailValue::Number(count)), "signout.audit_failed");
    Ok(no_content(vec![cookies::delete_session()]))
}

// ---------------------------------------------------------------------------------------------------------------------
// The part
// ---------------------------------------------------------------------------------------------------------------------

/// The routes under `/api/me`, as one part for [`crate::http::router::build`].
pub fn parts(services: Arc<MeServices>) -> Vec<Part> {
    let router = Router::new()
        .route("/api/me", get(me_route))
        .route("/api/me/password", post(password_route))
        .route("/api/me/totp", post(totp_route))
        .route("/api/me/passkeys", post(passkeys_route))
        .route("/api/me/passkeys/{id}", delete(passkey_remove_route))
        .route("/api/me/passphrase", put(passphrase_route))
        .route("/api/me/recovery", post(recovery_route))
        .route("/api/me/sessions", delete(sessions_end_all_route))
        .route("/api/me/sessions/{handle}", delete(session_end_route))
        .with_state(services);
    vec![Part { access: Access::SessionFresh, router }]
}

#[allow(dead_code)]
fn _store_is_used(_: &Store) {}
