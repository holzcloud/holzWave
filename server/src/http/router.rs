//! The router: the part-based assembly of the HTTP application, the access kinds, the session middleware, the extractors,
//! `GET /api/session` and the error vocabulary.
//!
//! A route module provides a plain [`axum::Router`] and says under which [`Access`] it is mounted. [`build`] puts the checks
//! of that access in front of every route of the part, so a new route cannot forget one: the check is the mount point.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;
use serde_json::json;

use crate::auth::session::SessionError;
use crate::config::Sessions;
use crate::store::{Account, Session};

use super::app::{App, StoreState};
use super::cookies;
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
            ApiError::BadRequest { .. } | ApiError::MethodNotAllowed => "bad_request",
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

/// The checks of an access kind, in front of every route of a part.
async fn guard(State(guard): State<Guard>, mut request: Request, next: Next) -> Response {
    let outcome = match cookies::read_session(request.headers()) {
        None => Outcome::Anonymous,
        Some(value) => {
            let client = request.extensions().get::<ClientAddr>().map_or(IpAddr::from([0u8; 4]), |c| c.0);
            resolve_value(guard.app.clone(), value, client).await
        }
    };
    match outcome {
        Outcome::Busy => return ApiError::Busy.into_response(),
        Outcome::Live(authed) => {
            request.extensions_mut().insert(*authed);
        }
        Outcome::Stale => match guard.access {
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
            if !matches!(guard.access, Access::Public | Access::Socket) {
                return ApiError::Invalid.into_response();
            }
        }
    }
    next.run(request).await
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

/// Gives a bare 405 from a route (which has no body) the plain JSON error: no `Allow` list, no detail.
async fn plain_errors(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if response.status() == StatusCode::METHOD_NOT_ALLOWED && !response.headers().contains_key(header::CONTENT_TYPE) {
        return ApiError::MethodNotAllowed.into_response();
    }
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
    root.fallback(not_found)
        .layer(Extension(app.clone()))
        .layer(middleware::from_fn(plain_errors))
        .layer(middleware::from_fn_with_state(Arc::new(SecurityHeaders::new(&app.origin)), headers::layer))
}
