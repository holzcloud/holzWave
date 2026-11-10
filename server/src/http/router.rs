//! The router: the part-based assembly of the HTTP application, the access kinds, the session middleware, the extractors,
//! `GET /api/session` and the error vocabulary.
//!
//! A route module provides a plain [`axum::Router`] and says under which [`Access`] it is mounted. [`build`] puts the checks
//! of that access in front of every route of the part, so a new route cannot forget one: the check is the mount point.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::auth::session::SessionError;
use crate::config::Sessions;
use crate::proxy::Mode;
use crate::device::{DeviceToken, TOUCH_INTERVAL};
use crate::store::{Account, Device, Role, Session, TokenHash};

use super::app::{App, StoreState};
use super::cookies;
use super::csrf::{self, BodyKind};
use super::headers::{self, SecurityHeaders};

/// The address of the peer of the connection, put into every request by the accept loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerAddr(pub IpAddr);

/// The address of the client after the proxy rule: the peer itself, or the address a trusted proxy reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAddr(pub IpAddr);

/// What a part of the application needs from the request before its handlers see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// No session needed. The handler may ask for the session through [`MaybeAuthed`].
    Public,
    /// A live session; every state-changing method also needs the anti-forgery token.
    Session,
    /// As [`Access::Session`], and every state-changing method needs a sign-in no older than the step-up time.
    SessionFresh,
    /// No refusal here: the handler gets a verdict for the Origin, the Host and the session, and closes with the code the
    /// page understands.
    Socket,
    /// As [`Access::Session`], with a raw `application/octet-stream` body.
    Upload,
    /// A device of a person: one `Authorization: Bearer` header with an unexpired token of an active account. No cookie is read,
    /// no Origin rule and no anti-forgery token apply (a bearer token is not ambient authority), and the body is left to the
    /// handler, which reads it with its own cap.
    Device,
    /// A native client that has no credential yet (the start of its sign-in): no cookie, no Origin rule, JSON only, with the
    /// body read here up to the cap.
    DeviceOpen,
}

/// A group of routes with the access they share.
pub struct Part {
    /// The checks that run before the routes.
    pub access: Access,
    /// The routes.
    pub router: Router,
}

/// Why a request ends in an error answer. The answer is `{"code":...}` and nothing else but the members named here: no
/// sentence, no internal message, no path and no input byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// 400 `bad_request`, with the name of the field when one field is at fault.
    BadRequest {
        /// The field.
        field: Option<&'static str>,
    },
    /// 401 `invalid`: no session, a session that ended, or a sign-in that failed.
    Invalid,
    /// 403 `forbidden`, with `step_up` when a recent sign-in would make it pass.
    Forbidden {
        /// Whether signing in again would help.
        step_up: bool,
    },
    /// 404 `not_found`.
    NotFound,
    /// 405: the method is not served here. The code is `bad_request`, because the vocabulary has no 405.
    MethodNotAllowed,
    /// 421: the request came to a name that is not the public one. The code is `bad_request`, because the vocabulary has no
    /// 421.
    Misdirected,
    /// 409 `conflict`, with the field when one field is at fault.
    Conflict {
        /// The field.
        field: Option<&'static str>,
    },
    /// 410 `gone`: a token that is unknown, expired or used.
    Gone,
    /// 423 `locked`: the secret store is locked.
    Locked,
    /// 429 `throttled`, with the seconds to wait.
    Throttled {
        /// Seconds until a new try is allowed.
        retry_seconds: u64,
    },
    /// 503 `busy`.
    Busy,
}

impl ApiError {
    /// The status.
    pub fn status(self) -> StatusCode {
        match self {
            ApiError::BadRequest { .. } => StatusCode::BAD_REQUEST,
            ApiError::Invalid => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden { .. } => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            ApiError::Misdirected => StatusCode::MISDIRECTED_REQUEST,
            ApiError::Conflict { .. } => StatusCode::CONFLICT,
            ApiError::Gone => StatusCode::GONE,
            ApiError::Locked => StatusCode::from_u16(423).unwrap_or(StatusCode::FORBIDDEN),
            ApiError::Throttled { .. } => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Busy => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The `code` member.
    pub fn code(self) -> &'static str {
        match self {
            ApiError::BadRequest { .. } | ApiError::MethodNotAllowed | ApiError::Misdirected => "bad_request",
            ApiError::Invalid => "invalid",
            ApiError::Forbidden { .. } => "forbidden",
            ApiError::NotFound => "not_found",
            ApiError::Conflict { .. } => "conflict",
            ApiError::Gone => "gone",
            ApiError::Locked => "locked",
            ApiError::Throttled { .. } => "throttled",
            ApiError::Busy => "busy",
        }
    }

    fn body(self) -> serde_json::Value {
        match self {
            ApiError::BadRequest { field: Some(field) } | ApiError::Conflict { field: Some(field) } => {
                json!({ "code": self.code(), "field": field })
            }
            ApiError::Forbidden { step_up: true } => json!({ "code": self.code(), "step_up": true }),
            ApiError::Throttled { retry_seconds } => json!({ "code": self.code(), "retry_seconds": retry_seconds }),
            _ => json!({ "code": self.code() }),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status(), Json(self.body())).into_response();
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// A request with a live session, as the session middleware found it. `Debug` prints a fixed marker.
#[derive(Clone)]
pub struct Authed {
    /// The session row, as touched by this request.
    pub session: Session,
    /// The account row.
    pub account: Account,
    /// The anti-forgery token of the session.
    pub csrf: String,
    /// The client address after the proxy rule.
    pub client: IpAddr,
    /// Whether the secret store of the account is locked.
    pub locked: bool,
    /// Seconds to the idle or absolute expiry, whichever is first, as seen when the request arrived (before it touched the
    /// session).
    pub seconds: i64,
    /// Seconds left of the recent sign-in window; 0 when none.
    pub step_up: i64,
    /// Whether the sign-in is recent enough for a sensitive change.
    pub fresh: bool,
}

impl fmt::Debug for Authed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Authed(..)")
    }
}

/// The session of a request on a part that does not need one: `None` for no cookie or a cookie that is not a live session.
#[derive(Debug, Clone)]
pub struct MaybeAuthed(pub Option<Authed>);

/// Set on a request whose cookie named a session that is not alive: the handler may delete the cookie.
#[derive(Debug, Clone, Copy)]
pub struct StaleCookie;

impl<S: Send + Sync> FromRequestParts<S> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Authed, ApiError> {
        parts.extensions.get::<Authed>().cloned().ok_or(ApiError::Invalid)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for MaybeAuthed {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<MaybeAuthed, Self::Rejection> {
        Ok(MaybeAuthed(parts.extensions.get::<Authed>().cloned()))
    }
}

/// A request of an administrator: `Authed` with the role `admin`, otherwise 403 `forbidden` (no `step_up`).
#[derive(Debug, Clone)]
pub struct Admin(pub Authed);

impl<S: Send + Sync> FromRequestParts<S> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Admin, ApiError> {
        let authed = parts.extensions.get::<Authed>().cloned().ok_or(ApiError::Invalid)?;
        if authed.account.role != Role::Admin {
            return Err(ApiError::Forbidden { step_up: false });
        }
        Ok(Admin(authed))
    }
}

/// A request whose sign-in is no older than the step-up time, otherwise 403 `forbidden` with `step_up`. It is the explicit
/// form of [`Access::SessionFresh`], for a handler that wants the rule on one method only.
#[derive(Debug, Clone)]
pub struct Fresh(pub Authed);

impl<S: Send + Sync> FromRequestParts<S> for Fresh {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Fresh, ApiError> {
        let authed = parts.extensions.get::<Authed>().cloned().ok_or(ApiError::Invalid)?;
        if !authed.fresh {
            return Err(ApiError::Forbidden { step_up: true });
        }
        Ok(Fresh(authed))
    }
}

/// A request of a device, as the device guard found it. `Debug` prints a fixed marker.
#[derive(Clone)]
pub struct DeviceAuthed {
    /// The device row, as touched by this request.
    pub device: Device,
    /// The account the device belongs to.
    pub account: Account,
    /// The client address after the proxy rule.
    pub client: IpAddr,
    /// The SHA-256 of the token the request carried, which a long-lived socket looks the device up by again. Not the token.
    pub token_hash: TokenHash,
}

impl fmt::Debug for DeviceAuthed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceAuthed(..)")
    }
}

impl<S: Send + Sync> FromRequestParts<S> for DeviceAuthed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<DeviceAuthed, ApiError> {
        parts.extensions.get::<DeviceAuthed>().cloned().ok_or(ApiError::Invalid)
    }
}

/// What the router found out about a request on a [`Access::Socket`] part. The router refuses nothing there; the handler
/// turns the verdict into the close code the page understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketVerdict {
    /// The `Origin` is the public origin, exactly one header.
    pub origin_ok: bool,
    /// The `Host` is the public authority.
    pub host_ok: bool,
    /// The request has a live session.
    pub session: bool,
}

impl SocketVerdict {
    /// Whether the upgrade may go on: all three hold.
    pub fn allowed(self) -> bool {
        self.origin_ok && self.host_ok && self.session
    }
}

/// A JSON body, read strictly: the type decides which members are allowed, and every failure (an empty body, a body that is
/// cut off, a wrong type, an unknown member, nesting that is too deep, bytes that are not UTF-8, a body over the cap) is 400
/// `bad_request` with the same plain body, without a word about what was wrong.
#[derive(Debug, Clone)]
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, _state: &S) -> Result<ApiJson<T>, ApiError> {
        let cap = request
            .extensions()
            .get::<Arc<App>>()
            .map_or(crate::limits::Limits::DEFAULT.api_body_bytes, |app| app.limits.api_body_bytes);
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        let bytes = axum::body::to_bytes(request.into_body(), cap).await.map_err(|_| ApiError::BadRequest { field: None })?;
        serde_json::from_slice(&bytes).map(ApiJson).map_err(|_| ApiError::BadRequest { field: None })
    }
}

/// Seconds from `now` to the idle or the absolute end of `session`, whichever is first; never negative.
pub fn seconds_left(session: &Session, lifetimes: &Sessions, now: i64) -> i64 {
    let idle = i64::try_from(lifetimes.idle.as_secs()).unwrap_or(i64::MAX);
    let idle_ends = session.last_seen.saturating_add(idle);
    idle_ends.min(session.absolute_expires).saturating_sub(now).max(0)
}

/// Seconds from `now` to the end of the recent sign-in window of `session`; 0 when it is over.
pub fn step_up_left(session: &Session, lifetimes: &Sessions, now: i64) -> i64 {
    let window = i64::try_from(lifetimes.step_up.as_secs()).unwrap_or(i64::MAX);
    session.last_auth.saturating_add(window).saturating_sub(now).max(0)
}

/// What the cookie of a request amounted to.
enum Outcome {
    /// There is no usable cookie.
    Anonymous,
    /// The cookie names a session that is not alive.
    Stale,
    /// A live session.
    Live(Box<Authed>),
    /// The store failed.
    Busy,
}

/// Looks the cookie value up, touches the session and reads the account. It blocks on the database, so it runs on the
/// blocking pool.
fn resolve_blocking(app: &App, value: &str, client: IpAddr) -> Outcome {
    let manager = &app.session_manager;
    let now = app.clock.unix();
    let mut session = match manager.lookup(value, now) {
        Ok(session) => session,
        Err(SessionError::Unknown | SessionError::Expired | SessionError::AccountDisabled) => return Outcome::Stale,
        Err(_) => return Outcome::Busy,
    };
    let seconds = seconds_left(&session, &app.sessions, now);
    let step_up = step_up_left(&session, &app.sessions, now);
    let fresh = manager.step_up_fresh(&session, now);
    match manager.touch(&mut session, now) {
        Ok(_) => {}
        Err(SessionError::Unknown) => return Outcome::Stale,
        Err(_) => return Outcome::Busy,
    }
    let account = match app.store.account_get(session.account_id) {
        Ok(Some(account)) => account,
        Ok(None) => return Outcome::Stale,
        Err(_) => return Outcome::Busy,
    };
    let locked = app.view.state(account.id) == StoreState::Locked;
    let csrf = app.csrf_token(&session);
    Outcome::Live(Box::new(Authed { session, account, csrf, client, locked, seconds, step_up, fresh }))
}

async fn resolve_value(app: Arc<App>, value: String, client: IpAddr) -> Outcome {
    match tokio::task::spawn_blocking(move || resolve_blocking(&app, &value, client)).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Busy,
    }
}

/// The state of the middleware of one part.
#[derive(Clone)]
struct Guard {
    app: Arc<App>,
    access: Access,
}

/// The body cap of a JSON route in bytes.
fn body_cap(app: &App) -> usize {
    usize::try_from(app.limits.api_body_bytes).unwrap_or(usize::MAX)
}

/// The checks of an access kind, in front of every route of a part, in this order: the Origin and the content type of a
/// change, the declared size, the session, the anti-forgery token, the body.
async fn guard(State(guard): State<Guard>, mut request: Request, next: Next) -> Response {
    let app = &guard.app;
    let access = guard.access;
    let changing = csrf::is_state_changing(request.method());
    let cap = body_cap(app);

    // The two kinds of a native client never look at a cookie, so they are handled before anything of the session path.
    match access {
        Access::Device => return device_guard(app, request, next).await,
        Access::DeviceOpen => return open_guard(changing, cap, request, next).await,
        _ => {}
    }

    if changing && access != Access::Socket {
        if !csrf::change_origin_ok(&app.origin, request.headers()) {
            return ApiError::Forbidden { step_up: false }.into_response();
        }
        let kind = if access == Access::Upload { BodyKind::Octet } else { BodyKind::Json };
        if !csrf::content_type_ok(request.headers(), kind) {
            return ApiError::BadRequest { field: None }.into_response();
        }
        if access != Access::Upload {
            match csrf::declared_length(request.headers()) {
                Ok(Some(length)) if length > cap as u64 => return ApiError::BadRequest { field: None }.into_response(),
                Err(_) => return ApiError::BadRequest { field: None }.into_response(),
                _ => {}
            }
        }
    }

    let outcome = match cookies::read_session(request.headers()) {
        None => Outcome::Anonymous,
        Some(value) => {
            let client = request.extensions().get::<ClientAddr>().map_or(IpAddr::from([0u8; 4]), |c| c.0);
            resolve_value(app.clone(), value, client).await
        }
    };
    match outcome {
        Outcome::Busy => return ApiError::Busy.into_response(),
        Outcome::Live(authed) => {
            if changing
                && matches!(access, Access::Session | Access::SessionFresh | Access::Upload)
                && !csrf::token_header_ok(request.headers(), &authed.csrf)
            {
                return ApiError::Forbidden { step_up: false }.into_response();
            }
            if changing && access == Access::SessionFresh && !authed.fresh {
                return ApiError::Forbidden { step_up: true }.into_response();
            }
            request.extensions_mut().insert(*authed);
        }
        Outcome::Stale => match access {
            Access::Public | Access::Socket => {
                request.extensions_mut().insert(StaleCookie);
            }
            _ => {
                let mut response = ApiError::Invalid.into_response();
                response.headers_mut().append(header::SET_COOKIE, cookies::delete_session());
                return response;
            }
        },
        Outcome::Anonymous => {
            if !matches!(access, Access::Public | Access::Socket) {
                return ApiError::Invalid.into_response();
            }
        }
    }

    if access == Access::Socket {
        let verdict = SocketVerdict {
            origin_ok: csrf::origin_exact(&app.origin, request.headers()),
            host_ok: csrf::host_ok(&app.origin, request.headers(), request.uri()),
            session: request.extensions().get::<Authed>().is_some(),
        };
        request.extensions_mut().insert(verdict);
    }

    if changing && access != Access::Socket && access != Access::Upload {
        request = match buffer_body(request, cap).await {
            Ok(request) => request,
            Err(refusal) => return refusal.into_response(),
        };
    }
    next.run(request).await
}

/// Reads the whole body, up to the cap, so that a streamed body over the cap is refused whether or not the handler would have
/// read it, and a declared length that lies is refused too. The request goes on with the bytes as its body.
async fn buffer_body(request: Request, cap: usize) -> Result<Request, ApiError> {
    let declared = csrf::declared_length(request.headers()).ok().flatten();
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, cap).await else {
        return Err(ApiError::BadRequest { field: None });
    };
    if declared.is_some_and(|length| length != bytes.len() as u64) {
        return Err(ApiError::BadRequest { field: None });
    }
    Ok(Request::from_parts(parts, Body::from(bytes)))
}

/// The checks of [`Access::DeviceOpen`]: a change is JSON with a declared length inside the cap, and its body is read here. No
/// cookie is looked at and no Origin rule applies, because the caller is a native client.
async fn open_guard(changing: bool, cap: usize, request: Request, next: Next) -> Response {
    if !changing {
        return next.run(request).await;
    }
    if !csrf::content_type_ok(request.headers(), BodyKind::Json) {
        return ApiError::BadRequest { field: None }.into_response();
    }
    match csrf::declared_length(request.headers()) {
        Ok(Some(length)) if length > cap as u64 => return ApiError::BadRequest { field: None }.into_response(),
        Err(_) => return ApiError::BadRequest { field: None }.into_response(),
        _ => {}
    }
    match buffer_body(request, cap).await {
        Ok(request) => next.run(request).await,
        Err(refusal) => refusal.into_response(),
    }
}

/// The token of the one `Authorization: Bearer <token>` header of a request. Two headers, another scheme or anything but the
/// token after the scheme is no token.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let mut all = headers.get_all(header::AUTHORIZATION).iter();
    let one = all.next()?;
    if all.next().is_some() {
        return None;
    }
    let (scheme, token) = one.to_str().ok()?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then_some(token)
}

/// What the token of a device amounted to.
enum DeviceOutcome {
    /// No such token, or one that ended, or an account that is not active.
    Unknown,
    /// A device of an active account.
    Live(Box<(Device, Account)>),
    /// The store failed.
    Busy,
}

/// Looks the token up and, at most once a minute, writes that the device was seen. It blocks on the database, so it runs on the
/// blocking pool.
fn resolve_device(app: &App, hash: &TokenHash) -> DeviceOutcome {
    let now = app.clock.unix();
    let found = app.store.read(|tx| match tx.device_lookup(hash, now)? {
        None => Ok(None),
        Some(device) => Ok(tx.account_get(device.account_id)?.map(|account| (device, account))),
    });
    let (mut device, account) = match found {
        Ok(Some(found)) => found,
        Ok(None) => return DeviceOutcome::Unknown,
        Err(_) => return DeviceOutcome::Busy,
    };
    if now.saturating_sub(device.last_seen) >= TOUCH_INTERVAL {
        let idle = i64::try_from(app.devices.idle.as_secs()).unwrap_or(i64::MAX);
        match app.store.write(|tx| tx.device_touch(device.id, now, idle)) {
            Ok(true) => {
                device.last_seen = now;
                device.expires_at = now.saturating_add(idle);
            }
            Ok(false) => {}
            Err(_) => return DeviceOutcome::Busy,
        }
    }
    DeviceOutcome::Live(Box::new((device, account)))
}

/// The checks of [`Access::Device`]. The cookie header is never read here: a session cookie cannot stand in for the token, and
/// the answer never sets or deletes one. No Origin and no anti-forgery check, because a bearer token is not sent by the
/// browser on its own.
async fn device_guard(app: &Arc<App>, mut request: Request, next: Next) -> Response {
    let Some(text) = bearer_token(request.headers()) else { return ApiError::Invalid.into_response() };
    let Ok(token) = DeviceToken::parse(text) else { return ApiError::Invalid.into_response() };
    let hash = token.hash();
    drop(token);
    let client = request.extensions().get::<ClientAddr>().map_or(IpAddr::from([0u8; 4]), |c| c.0);
    let worker = app.clone();
    let outcome = match tokio::task::spawn_blocking(move || resolve_device(&worker, &hash)).await {
        Ok(outcome) => outcome,
        Err(_) => DeviceOutcome::Busy,
    };
    match outcome {
        DeviceOutcome::Unknown => ApiError::Invalid.into_response(),
        DeviceOutcome::Busy => ApiError::Busy.into_response(),
        DeviceOutcome::Live(found) => {
            let (device, account) = *found;
            request.extensions_mut().insert(DeviceAuthed { device, account, client, token_hash: hash });
            next.run(request).await
        }
    }
}

/// What a request has to pass before any part sees it, in every mode: the proxy rule, the Host, the method.
async fn edge(State(app): State<Arc<App>>, mut request: Request, next: Next) -> Response {
    let peer = request.extensions().get::<PeerAddr>().map_or(IpAddr::from([0u8; 4]), |p| p.0);
    let client = match client_address(&app, peer, request.headers()) {
        Ok(ip) => ip,
        Err(()) => return ApiError::BadRequest { field: None }.into_response(),
    };
    if !csrf::host_ok(&app.origin, request.headers(), request.uri()) {
        return ApiError::Misdirected.into_response();
    }
    if matches!(*request.method(), Method::OPTIONS | Method::TRACE | Method::CONNECT) {
        return ApiError::MethodNotAllowed.into_response();
    }
    // A path of a feature that is off is answered by the function that answers a path that does not exist, before any part,
    // and so before any credential is read: a probe cannot tell the two apart.
    if app.features.is_closed(request.uri().path()) {
        return not_found().await.into_response();
    }
    request.extensions_mut().insert(ClientAddr(client));
    next.run(request).await
}

/// The address of the client by the proxy rule: forwarded headers count only in the proxy mode and only from a trusted
/// proxy. A forwarded value that is not plain text is the same as a wrong one.
fn client_address(app: &App, peer: IpAddr, headers: &axum::http::HeaderMap) -> Result<IpAddr, ()> {
    if app.mode != Mode::Proxy {
        return Ok(peer.to_canonical());
    }
    let lines = |name: &str| -> Vec<&str> {
        headers.get_all(name).iter().map(|value| value.to_str().unwrap_or("\u{fffd}")).collect()
    };
    app.proxies.client_address(app.mode, peer, &lines("x-forwarded-for"), &lines("x-forwarded-proto")).map_err(|_| ())
}

// ---------------------------------------------------------------------------------------------------------------------
// GET /api/session
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Serialize)]
struct OidcMethod<'a> {
    id: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct Methods<'a> {
    passkey: bool,
    password: bool,
    oidc: Vec<OidcMethod<'a>>,
}

#[derive(Serialize)]
struct Anonymous<'a> {
    state: &'static str,
    host: &'a str,
    bootstrap: bool,
    methods: Methods<'a>,
}

#[derive(Serialize)]
struct AccountView<'a> {
    id: i64,
    username: &'a str,
    display_name: &'a str,
    role: &'static str,
}

#[derive(Serialize)]
struct Known<'a> {
    state: &'static str,
    host: &'a str,
    csrf: &'a str,
    account: AccountView<'a>,
    seconds: i64,
    step_up: i64,
}

async fn session_handler(
    Extension(app): Extension<Arc<App>>,
    MaybeAuthed(who): MaybeAuthed,
    stale: Option<Extension<StaleCookie>>,
) -> Response {
    let host = app.origin.authority();
    let mut response = match who {
        Some(authed) => {
            let state = if authed.locked { "locked" } else { "signed_in" };
            Json(Known {
                state,
                host,
                csrf: &authed.csrf,
                account: AccountView {
                    id: authed.account.id.get(),
                    username: &authed.account.username,
                    display_name: &authed.account.display_name,
                    role: authed.account.role.as_str(),
                },
                seconds: authed.seconds,
                step_up: authed.step_up,
            })
            .into_response()
        }
        None => {
            let store = app.store.clone();
            let admins = tokio::task::spawn_blocking(move || store.read(|tx| tx.admin_count_active())).await;
            let bootstrap = match admins {
                Ok(Ok(count)) => count == 0,
                _ => return ApiError::Busy.into_response(),
            };
            Json(Anonymous {
                state: "anonymous",
                host,
                bootstrap,
                methods: Methods {
                    passkey: app.auth.passkey,
                    password: app.auth.password_totp,
                    oidc: app.auth.oidc.iter().map(|p| OidcMethod { id: &p.id, name: &p.name }).collect(),
                },
            })
            .into_response()
        }
    };
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if stale.is_some() {
        response.headers_mut().append(header::SET_COOKIE, cookies::delete_session());
    }
    response
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// Gives every 405 the plain JSON error and no `Allow` list: the router does not describe itself. The method router adds the
/// list to every answer it wraps, so a 401 for a wrong method on a route that needs a session carried it too; it is removed
/// from every answer, whatever the status.
async fn plain_errors(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED && !response.headers().contains_key(header::CONTENT_TYPE) {
        return ApiError::MethodNotAllowed.into_response();
    }
    response.headers_mut().remove(header::ALLOW);
    response
}

/// The part the router serves itself: `GET /api/session`.
fn session_part() -> Part {
    Part { access: Access::Public, router: Router::new().route("/api/session", get(session_handler)) }
}

/// Assembles the application from the parts of the route modules. Every response, on every route and every error, carries
/// the fixed security headers.
pub fn build(app: Arc<App>, parts: Vec<Part>) -> Router {
    let mut root: Router = Router::new();
    for part in parts.into_iter().chain(std::iter::once(session_part())) {
        let state = Guard { app: app.clone(), access: part.access };
        root = root.merge(part.router.route_layer(middleware::from_fn_with_state(state, guard)));
    }
    // The routes are the fallback of an outer router, so that the layers below see the final answer of every route, after the
    // method router has added what it adds (for example the `Allow` list of a 405).
    Router::new()
        .fallback_service(root.fallback(not_found))
        .layer(Extension(app.clone()))
        .layer(middleware::from_fn_with_state(app.clone(), edge))
        .layer(middleware::from_fn(plain_errors))
        .layer(middleware::from_fn_with_state(Arc::new(SecurityHeaders::new(&app.origin)), headers::layer))
}
