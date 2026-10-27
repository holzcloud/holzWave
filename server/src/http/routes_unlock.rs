//! The route that unlocks the secret store of a person after a restart, and the gate the sign-in routes tell about a new
//! session.
//!
//! The secret store lives on the worker side. The routes never decide whether it opens: after a sign-in they hand the key
//! material of the method that was used to an [`UnlockGate`] and go on whatever the gate says, and the explicit unlock hands
//! the typed secret to the same gate and reports its verdict. The implementation of the gate, not the route, writes the
//! `secrets.*` audit lines.

use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::auth::passkey::PrfMaterial;
use crate::auth::signin::UnlockMaterial;
use crate::auth::throttle::{Policy, Throttle, scoped_key};
use crate::http::app::StoreView;
use crate::http::router::{Access, ApiError, ApiJson, Authed, Part};
use crate::proxy::key_of;
use crate::store::AccountId;

use super::routes_auth::{AuthServices, GATE_TIMEOUT, Object, Refusal, answer, blocking};

/// A boxed future, so that the gate can be held as a trait object.
pub type GateFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The key material a sign-in produced, by the method that produced it. Not `Clone`; every variant wipes itself.
pub enum Material {
    /// The key-encryption key of the password.
    Password(UnlockMaterial),
    /// The credential, the PRF output and the salt of a passkey.
    Passkey(PrfMaterial),
    /// Single sign-on, or a passkey that sent no usable PRF output: nothing that could open the store.
    None,
}

impl Material {
    /// A word for the kind, for tests and diagnostics: `password`, `passkey` or `none`.
    pub fn kind(&self) -> &'static str {
        match self {
            Material::Password(_) => "password",
            Material::Passkey(_) => "passkey",
            Material::None => "none",
        }
    }
}

impl std::fmt::Debug for Material {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Material(..)")
    }
}

/// The secret a person typed to unlock the store. Borrowed from the request, printed as a marker.
pub enum UnlockSecret<'a> {
    /// The passphrase of the store.
    Passphrase(&'a str),
    /// A recovery code.
    RecoveryCode(&'a str),
}

impl std::fmt::Debug for UnlockSecret<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnlockSecret(..)")
    }
}

/// Why the gate did not do what it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// The secret does not open the store.
    Refused,
    /// The worker side could not be reached or failed.
    Unavailable,
}

/// The seam between the sign-in routes and the secret store of the worker side. It is also the [`StoreView`] the application
/// state reads, so one object serves both.
pub trait UnlockGate: StoreView {
    /// A person signed in; `material` is what the method produced. A failure changes nothing for the sign-in: the store stays
    /// locked and the page shows the unlock dialog.
    fn session_started(
        &self,
        account: AccountId,
        material: Material,
    ) -> GateFuture<'_, Result<(), GateError>>;

    /// An explicit unlock with a typed secret.
    fn unlock_with<'a>(
        &'a self,
        account: AccountId,
        secret: UnlockSecret<'a>,
    ) -> GateFuture<'a, Result<(), GateError>>;
}

/// The most bytes of a passphrase that are accepted.
const MAX_PASSPHRASE_BYTES: usize = 256;

/// The most characters of a recovery code that are accepted.
const MAX_RECOVERY_CHARS: usize = 64;

/// The throttle scope of the unlock attempts of one account. It is neither `a` nor `i`, the scopes of a sign-in.
const ACCOUNT_SCOPE: &str = "ua";

/// The throttle scope of the unlock attempts from one client address.
const ADDRESS_SCOPE: &str = "ui";

/// The throttle key of the unlock attempts of one account. Its own space: it never equals a sign-in key.
pub fn unlock_account_key(account: AccountId) -> String {
    scoped_key(ACCOUNT_SCOPE, &account.get().to_be_bytes())
}

/// The throttle key of the unlock attempts from one client address (an IPv6 address by its /64, as everywhere). Its own space:
/// it never equals a sign-in key.
pub fn unlock_address_key(addr: IpAddr) -> String {
    scoped_key(ADDRESS_SCOPE, key_of(addr).to_string().as_bytes())
}

struct UnlockState {
    services: Arc<AuthServices>,
    throttle: Throttle,
}

/// The members of the unlock body. Each is any JSON value here, so that a member of the wrong type is told apart from a body
/// that is no object, and answers with the name of the member.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnlockBody {
    #[serde(default)]
    passphrase: Option<serde_json::Value>,
    #[serde(default)]
    recovery_code: Option<serde_json::Value>,
}

/// The one secret of a body, wiped when dropped.
enum Typed {
    Passphrase(Zeroizing<String>),
    RecoveryCode(Zeroizing<String>),
}

fn bad(field: &'static str) -> ApiError {
    ApiError::BadRequest { field: Some(field) }
}

/// Exactly one of the two members, a text within its bound. Anything else names the member at fault.
fn typed(body: UnlockBody) -> Result<Typed, ApiError> {
    match (body.passphrase, body.recovery_code) {
        (Some(serde_json::Value::String(text)), None) => {
            let text = Zeroizing::new(text);
            if text.is_empty() || text.len() > MAX_PASSPHRASE_BYTES {
                return Err(bad("passphrase"));
            }
            Ok(Typed::Passphrase(text))
        }
        (None, Some(serde_json::Value::String(text))) => {
            let text = Zeroizing::new(text);
            if text.is_empty() || text.chars().count() > MAX_RECOVERY_CHARS {
                return Err(bad("recovery_code"));
            }
            Ok(Typed::RecoveryCode(text))
        }
        (None, Some(_)) => Err(bad("recovery_code")),
        _ => Err(bad("passphrase")),
    }
}

/// `POST /auth/unlock`: hands one typed secret to the gate and reports its verdict. The attempts are throttled under keys of
/// their own, checked before the gate is asked, so a locked key never reaches it. The route writes no audit line: the gate
/// writes the `secrets.*` lines.
async fn unlock_route(
    State(state): State<Arc<UnlockState>>,
    authed: Authed,
    ApiJson(Object(body)): ApiJson<Object<UnlockBody>>,
) -> Result<Response, Refusal> {
    let secret = typed(body)?;
    let account = authed.account.id;
    let account_key = unlock_account_key(account);
    let address_key = unlock_address_key(authed.client);
    let now = state.services.app.clock.unix();

    let throttle = state.throttle.clone();
    let (a, i) = (account_key.clone(), address_key.clone());
    let locked = blocking(move || throttle.check(Some(&a), &i, now))
        .await?
        .map_err(|_| ApiError::Busy)?;
    if let Some(retry_seconds) = locked {
        return Err(ApiError::Throttled { retry_seconds }.into());
    }

    let offered = match &secret {
        Typed::Passphrase(text) => UnlockSecret::Passphrase(text.as_str()),
        Typed::RecoveryCode(text) => UnlockSecret::RecoveryCode(text.as_str()),
    };
    let verdict = tokio::time::timeout(
        GATE_TIMEOUT,
        state.services.gate.unlock_with(account, offered),
    )
    .await
    .unwrap_or(Err(GateError::Unavailable));
    drop(secret);

    match verdict {
        Ok(()) => {
            let throttle = state.throttle.clone();
            // A failure to forget the old failures only leaves them counting; the person is unlocked all the same.
            let _ = blocking(move || throttle.clear_account(&account_key)).await;
            Ok(answer(
                StatusCode::OK,
                &serde_json::json!({ "state": "unlocked" }),
                Vec::new(),
            ))
        }
        Err(GateError::Refused) => {
            let throttle = state.throttle.clone();
            // A failure that cannot be counted must not become a free guess.
            blocking(move || throttle.record_failure(Some(&account_key), &address_key, now))
                .await?
                .map_err(|_| ApiError::Busy)?;
            Err(ApiError::Invalid.into())
        }
        Err(GateError::Unavailable) => Err(ApiError::Busy.into()),
    }
}

/// The routes of the unlock: `POST /auth/unlock`, for a person with a session. It does not depend on a sign-in method.
pub fn parts(services: Arc<AuthServices>) -> Vec<Part> {
    let throttle = Throttle::new(
        services.app.store.clone(),
        Policy::from_limits(&services.app.limits),
    );
    let state = Arc::new(UnlockState { services, throttle });
    let router = Router::new()
        .route("/auth/unlock", post(unlock_route))
        .with_state(state);
    vec![Part {
        access: Access::Session,
        router,
    }]
}
