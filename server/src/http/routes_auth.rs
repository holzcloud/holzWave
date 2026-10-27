//! The routes of sign-in, sign-out, passkey and single sign-on ceremonies.
//!
//! The services that verify a password and a code, a passkey assertion or a single sign-on response are built and tested
//! elsewhere; this module puts them behind the routes of the contract. It decides what the browser sees and nothing else:
//!
//! - **No route makes a session.** A session value comes out of a service call that verified every factor; a value from a
//!   request (a cookie, a body, a query) is never turned into a session.
//! - **A failed step looks the same whatever the cause.** Every refusal of a sign-in step is the one `401 invalid` answer, with
//!   no cookie and no `Retry-After`; a lock is a `429` with the seconds; an overloaded derivation gate is a `503`.
//! - **A sign-in makes exactly one session.** The new value is set in the exact cookie of the cookie builder, the cookie of the
//!   half-finished step is deleted, and a session the browser still held is ended.
//! - **The secret store is told, not asked.** The key material of the method that was used goes to the [`UnlockGate`]; a
//!   failure of the gate does not fail the sign-in, and the material is in no answer, log or audit line.
//!
//! This file writes no log line on purpose: the services and the gate own their log and audit lines, and a route that logged
//! would be one more place a value could leak to.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use cookie::Cookie;
use serde::de::value::MapAccessDeserializer;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;
use zeroize::Zeroize;

use crate::auth::oidc::Http;
use crate::auth::oidc::{OidcCallback, OidcError, OidcGrant, OidcService, OidcStart};
use crate::auth::passkey::{PasskeyError, Passkeys};
use crate::auth::signin::{SignIn, SignInError};
use crate::http::app::App;
use crate::http::router::{Access, ApiError, ApiJson, Authed, ClientAddr, MaybeAuthed, Part};
use crate::store::AccountId;

use super::cookies::{self, MAX_COOKIE_HEADER_BYTES, MAX_COOKIE_PAIRS, MAX_TOKEN_CHARS, OIDC_COOKIE, PENDING_COOKIE};
use super::routes_unlock::{GateFuture, Material, UnlockGate};

/// How long a sign-in waits for the unlock gate before it goes on without it.
const GATE_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest `User-Agent` text that is looked at, in bytes. A real header is far shorter.
const MAX_AGENT_BYTES: usize = 512;

/// Where every failure of a single sign-on sends the browser. It is the only redirect target of this module besides the
/// address the single sign-on service builds, and nothing of the request is put into it.
const ERROR_LOCATION: &str = "/?error=oidc";

/// The most characters of a single sign-on provider id.
const MAX_PROVIDER_ID: usize = 32;

/// The most characters of the `prf` member that are looked at; a longer value is no PRF output.
const MAX_PRF_CHARS: usize = 64;

/// What a device is called when the header says nothing usable.
const UNKNOWN_DEVICE: &str = "Browser";

/// The single sign-on service as the routes use it, without its HTTP client type.
pub trait OidcFlow: Send + Sync {
    /// Starts a sign-in at the provider `id`.
    fn start<'a>(&'a self, id: &'a str, addr: IpAddr) -> GateFuture<'a, Result<OidcStart, OidcError>>;

    /// Finishes a sign-in from the callback of the provider.
    fn finish<'a>(
        &'a self,
        transaction: &'a str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &'a str,
    ) -> GateFuture<'a, Result<OidcGrant, OidcError>>;
}

impl<H: Http> OidcFlow for OidcService<H> {
    fn start<'a>(&'a self, id: &'a str, addr: IpAddr) -> GateFuture<'a, Result<OidcStart, OidcError>> {
        Box::pin(OidcService::start(self, id, addr))
    }

    fn finish<'a>(
        &'a self,
        transaction: &'a str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &'a str,
    ) -> GateFuture<'a, Result<OidcGrant, OidcError>> {
        Box::pin(OidcService::finish(self, transaction, callback, addr, device))
    }
}

/// The login page, as the callback of a successful single sign-on answers it.
pub trait LandingPage: Send + Sync {
    /// The bytes of the login page.
    fn login_page(&self) -> Bytes;
}

/// The services the sign-in routes call.
pub struct AuthServices {
    /// The application state; its configuration says which methods are on.
    pub app: Arc<App>,
    /// The password and code sign-in.
    pub signin: Arc<SignIn>,
    /// The passkey ceremonies.
    pub passkeys: Arc<Passkeys>,
    /// Single sign-on; `None` when no provider is configured.
    pub oidc: Option<Arc<dyn OidcFlow>>,
    /// The seam to the secret store.
    pub gate: Arc<dyn UnlockGate>,
    /// The login page.
    pub page: Arc<dyn LandingPage>,
}

type Shared = Arc<AuthServices>;

// ---------------------------------------------------------------------------------------------------------------------
// The device summary
// ---------------------------------------------------------------------------------------------------------------------

/// The browser and the system in a `User-Agent` text, as `Firefox on Linux`, from a closed list of names. A text that names
/// no browser, an empty text and a text longer than a real header are `Browser`. The header itself is never kept.
pub fn device_of(user_agent: &str) -> String {
    if user_agent.len() > MAX_AGENT_BYTES {
        return UNKNOWN_DEVICE.to_owned();
    }
    let has = |needle: &str| user_agent.contains(needle);
    let browser = if has("Edg/") || has("EdgA/") || has("EdgiOS/") || has("Edge/") {
        Some("Edge")
    } else if has("OPR/") || has("Opera") {
        Some("Opera")
    } else if has("Firefox/") || has("FxiOS/") {
        Some("Firefox")
    } else if has("Chrome/") || has("CriOS/") || has("Chromium/") {
        Some("Chrome")
    } else if has("Safari/") && has("Version/") {
        Some("Safari")
    } else {
        None
    };
    // Android and iOS agents also name Linux and macOS, so they are asked first.
    let system = if has("Android") {
        Some("Android")
    } else if has("iPhone") || has("iPad") || has("iPod") {
        Some("iOS")
    } else if has("Windows") {
        Some("Windows")
    } else if has("Macintosh") || has("Mac OS X") {
        Some("macOS")
    } else if has("CrOS") {
        Some("ChromeOS")
    } else if has("Linux") || has("X11") {
        Some("Linux")
    } else {
        None
    };
    match (browser, system) {
        (Some(browser), Some(system)) => format!("{browser} on {system}"),
        (Some(browser), None) => browser.to_owned(),
        (None, _) => UNKNOWN_DEVICE.to_owned(),
    }
}

/// The device summary of a request.
fn device_of_request(headers: &HeaderMap) -> String {
    let agent = headers.get(header::USER_AGENT).and_then(|value| value.to_str().ok()).unwrap_or("");
    device_of(agent)
}

// ---------------------------------------------------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------------------------------------------------

/// A refusal: the error vocabulary of the router, with the `Retry-After` header a lock needs.
pub(super) struct Refusal(pub(super) ApiError);

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let mut response = self.0.into_response();
        if let ApiError::Throttled { retry_seconds } = self.0 {
            response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(retry_seconds));
        }
        response
    }
}

impl From<ApiError> for Refusal {
    fn from(error: ApiError) -> Refusal {
        Refusal(error)
    }
}

impl From<SignInError> for Refusal {
    fn from(error: SignInError) -> Refusal {
        Refusal(match error {
            SignInError::Invalid => ApiError::Invalid,
            SignInError::Throttled { retry_seconds } => ApiError::Throttled { retry_seconds },
            SignInError::Busy | SignInError::Internal => ApiError::Busy,
        })
    }
}

impl From<PasskeyError> for Refusal {
    fn from(error: PasskeyError) -> Refusal {
        Refusal(match error {
            PasskeyError::Invalid | PasskeyError::TooMany | PasskeyError::NotFound => ApiError::Invalid,
            PasskeyError::Conflict => ApiError::Conflict { field: None },
            PasskeyError::Internal => ApiError::Busy,
        })
    }
}

/// An answer with a JSON body, `no-store` and the given cookies.
pub(super) fn answer(status: StatusCode, body: &serde_json::Value, cookies: Vec<HeaderValue>) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

/// A body that must be a JSON object. A struct that derives `Deserialize` also reads a JSON array of its members in order;
/// this wrapper refuses the array, so that `["alice","secret"]` is not a sign-in. Unknown members and a member named twice are
/// refused by the struct itself.
pub(super) struct Object<T>(pub(super) T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Object<T>, D::Error> {
        struct Reader<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for Reader<T> {
            type Value = T;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map))
            }
        }

        deserializer.deserialize_map(Reader(std::marker::PhantomData)).map(Object)
    }
}

/// Runs a blocking service call off the async threads.
pub(super) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, Refusal> {
    tokio::task::spawn_blocking(work).await.map_err(|_| Refusal(ApiError::Busy))
}

// ---------------------------------------------------------------------------------------------------------------------
// Cookies and the end of a sign-in
// ---------------------------------------------------------------------------------------------------------------------

/// The value of the cookie `name` of a request, read as strictly as the session cookie: nothing for a header that is too big,
/// too many pairs, a pair that does not parse, the name twice, or a value that is not a plain token.
fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut total = 0usize;
    let mut pairs = 0usize;
    let mut found: Option<String> = None;
    for line in headers.get_all(header::COOKIE) {
        total = total.saturating_add(line.len());
        if total > MAX_COOKIE_HEADER_BYTES {
            return None;
        }
        let text = line.to_str().ok()?;
        for parsed in Cookie::split_parse(text) {
            pairs += 1;
            if pairs > MAX_COOKIE_PAIRS {
                return None;
            }
            let cookie = parsed.ok()?;
            if cookie.name() == name {
                if found.is_some() {
                    return None;
                }
                found = Some(cookie.value().to_owned());
            }
        }
    }
    found.filter(|value| {
        !value.is_empty()
            && value.len() <= MAX_TOKEN_CHARS
            && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
    })
}

/// The cookie of the half-finished step, or the empty text (which no service accepts, and which they count as a failure).
fn pending_value(headers: &HeaderMap) -> String {
    read_cookie(headers, PENDING_COOKIE).unwrap_or_default()
}

/// What follows a verified sign-in: the session the browser still held is ended, and the unlock gate is told which key
/// material the method produced. A failure of either changes nothing for the person: the old session expires by itself, and
/// a store that stays locked shows the unlock dialog.
async fn after_sign_in(services: &Shared, held: Option<Authed>, account: AccountId, material: Material) {
    if let Some(old) = held {
        let sessions = services.app.session_manager.clone();
        let _ = tokio::task::spawn_blocking(move || sessions.end(&old.session)).await;
    }
    let _ = tokio::time::timeout(GATE_TIMEOUT, services.gate.session_started(account, material)).await;
}

// ---------------------------------------------------------------------------------------------------------------------
// The password and the code
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    username: String,
    password: String,
}

impl Drop for PasswordBody {
    fn drop(&mut self) {
        self.username.zeroize();
        self.password.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TotpBody {
    code: String,
}

impl Drop for TotpBody {
    fn drop(&mut self) {
        self.code.zeroize();
    }
}

/// `POST /auth/password`: the first step. A right name and password give the pending cookie and nothing else.
async fn password_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    ApiJson(Object(body)): ApiJson<Object<PasswordBody>>,
) -> Result<Response, Refusal> {
    let token = services.signin.begin_password(&body.username, &body.password, client.0).await?;
    let cookie = cookies::pending(token.expose()).ok_or(ApiError::Busy)?;
    Ok(answer(StatusCode::OK, &serde_json::json!({ "next": "totp" }), vec![cookie]))
}

/// `POST /auth/totp`: the second step. The only place a password sign-in makes a session.
async fn totp_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    ApiJson(Object(body)): ApiJson<Object<TotpBody>>,
) -> Result<Response, Refusal> {
    let token = pending_value(&headers);
    let device = device_of_request(&headers);
    let signin = services.signin.clone();
    let addr = client.0;
    let code = body.code.clone();
    let grant = blocking(move || signin.finish_totp(&token, &code, addr, &device)).await??;
    let cookie = cookies::session(grant.session.value.expose()).ok_or(ApiError::Busy)?;
    let account = grant.session.session.account_id;
    after_sign_in(&services, held, account, Material::Password(grant.unlock)).await;
    Ok(answer(StatusCode::OK, &serde_json::json!({ "next": "done" }), vec![cookie, cookies::delete_pending()]))
}

// ---------------------------------------------------------------------------------------------------------------------
// Passkeys
// ---------------------------------------------------------------------------------------------------------------------

/// A body with no member: `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasskeyFinishBody {
    /// The credential as the page read it from the browser. Any JSON value: what is not a credential is the service's `Invalid`.
    credential: serde_json::Value,
    /// The PRF output of the passkey, as 43 characters of unpadded base64url.
    #[serde(default)]
    prf: Option<String>,
}

/// `POST /auth/passkey/begin`: the request options for a username-less sign-in and the pending cookie of the challenge.
async fn passkey_begin_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    let passkeys = services.passkeys.clone();
    let addr = client.0;
    let (options, token) = blocking(move || passkeys.start_sign_in(addr)).await??;
    let options: serde_json::Value = serde_json::from_str(&options).map_err(|_| ApiError::Busy)?;
    let cookie = cookies::pending(token.expose()).ok_or(ApiError::Busy)?;
    Ok(answer(StatusCode::OK, &serde_json::json!({ "options": options }), vec![cookie]))
}

/// `POST /auth/passkey/finish`: verifies the assertion against the challenge of the pending cookie. A session is made by the
/// service and only after the assertion verified.
async fn passkey_finish_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    ApiJson(Object(body)): ApiJson<Object<PasskeyFinishBody>>,
) -> Result<Response, Refusal> {
    let token = pending_value(&headers);
    let device = device_of_request(&headers);
    let passkeys = services.passkeys.clone();
    let addr = client.0;
    let credential = body.credential.to_string();
    let mut prf = body.prf.filter(|text| text.len() <= MAX_PRF_CHARS);
    let signed = blocking(move || {
        let outcome = passkeys.finish_sign_in(&token, &credential, prf.as_deref(), addr, &device);
        if let Some(text) = prf.as_mut() {
            text.zeroize();
        }
        outcome
    })
    .await??;
    let cookie = cookies::session(signed.session.value.expose()).ok_or(ApiError::Busy)?;
    let material = signed.prf.map_or(Material::None, Material::Passkey);
    after_sign_in(&services, held, signed.account, material).await;
    Ok(answer(StatusCode::OK, &serde_json::json!({ "next": "done" }), vec![cookie, cookies::delete_pending()]))
}

// ---------------------------------------------------------------------------------------------------------------------
// Single sign-on
// ---------------------------------------------------------------------------------------------------------------------

/// Whether `id` is the shape of a provider id and names a configured provider.
fn provider_known(services: &AuthServices, id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PROVIDER_ID
        && id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        && services.app.auth.oidc.iter().any(|provider| provider.id == id)
}

/// The fixed redirect of a failed single sign-on: no reason, no input, the transaction cookie deleted.
fn oidc_failure() -> Response {
    let mut response = StatusCode::FOUND.into_response();
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, HeaderValue::from_static(ERROR_LOCATION));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.append(header::SET_COOKIE, cookies::delete_oidc());
    response
}

/// `GET /auth/oidc/{id}/start`: sends the browser to the provider and binds the transaction to it with the `Lax` cookie.
async fn oidc_start_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    id: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let Ok(Path(id)) = id else { return Err(ApiError::NotFound.into()) };
    let Some(flow) = services.oidc.as_ref().filter(|_| provider_known(&services, &id)) else {
        return Err(ApiError::NotFound.into());
    };
    let Ok(start) = flow.start(&id, client.0).await else { return Ok(oidc_failure()) };
    let (Ok(location), Some(cookie)) = (HeaderValue::from_str(&start.url), cookies::oidc(start.transaction.expose())) else {
        return Ok(oidc_failure());
    };
    let mut response = StatusCode::FOUND.into_response();
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, location);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.append(header::SET_COOKIE, cookie);
    Ok(response)
}

/// What the provider sent the browser back with. Unknown parameters are ignored; the service bounds every size.
#[derive(Deserialize, Default)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    iss: Option<String>,
}

/// `GET /auth/oidc/callback`: the one GET that changes state. A success answers the login page itself, so that the page's own
/// script moves to `/` from a same-site context and the `Strict` session cookie is sent; every failure is the fixed redirect.
async fn oidc_callback_route(
    State(services): State<Shared>,
    Extension(client): Extension<ClientAddr>,
    MaybeAuthed(held): MaybeAuthed,
    headers: HeaderMap,
    query: Result<Query<CallbackQuery>, QueryRejection>,
) -> Response {
    let Some(flow) = services.oidc.as_ref() else { return ApiError::NotFound.into_response() };
    // A query that cannot be read is a callback with nothing in it: the transaction is used up all the same.
    let query = query.map(|Query(query)| query).unwrap_or_default();
    let callback = OidcCallback { code: query.code, state: query.state, error: query.error, iss: query.iss };
    let transaction = read_cookie(&headers, OIDC_COOKIE).unwrap_or_default();
    let device = device_of_request(&headers);
    let Ok(grant) = flow.finish(&transaction, callback, client.0, &device).await else { return oidc_failure() };
    let Some(cookie) = cookies::session(grant.session.value.expose()) else { return oidc_failure() };
    after_sign_in(&services, held, grant.account, Material::None).await;
    let mut response = (StatusCode::OK, services.page.login_page()).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.append(header::SET_COOKIE, cookie);
    headers.append(header::SET_COOKIE, cookies::delete_oidc());
    response
}

// ---------------------------------------------------------------------------------------------------------------------
// The parts
// ---------------------------------------------------------------------------------------------------------------------

/// The sign-in, passkey, single sign-on, logout and step-up routes, as parts for [`crate::http::router::build`]. A method
/// that is off in the configuration of the application has no route: its paths answer like any unknown path.
pub fn parts(services: Arc<AuthServices>) -> Vec<Part> {
    let auth = &services.app.auth;
    let mut public: Router<Shared> = Router::new();
    let mut public_routes = 0usize;
    if auth.password_totp {
        public = public.route("/auth/password", post(password_route)).route("/auth/totp", post(totp_route));
        public_routes += 2;
    }
    if auth.passkey {
        public = public.route("/auth/passkey/begin", post(passkey_begin_route)).route("/auth/passkey/finish", post(passkey_finish_route));
        public_routes += 2;
    }
    if !auth.oidc.is_empty() && services.oidc.is_some() {
        public = public.route("/auth/oidc/{id}/start", get(oidc_start_route)).route("/auth/oidc/callback", get(oidc_callback_route));
        public_routes += 2;
    }
    let mut parts = Vec::new();
    // A part with no route would make the router builder panic, so only a part with a route is returned.
    if public_routes > 0 {
        parts.push(Part { access: Access::Public, router: public.with_state(services) });
    }
    parts
}
