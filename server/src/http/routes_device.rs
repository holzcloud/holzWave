//! The routes of the device features: the sign-in of an app that has no browser inside it (the device grant), what a device asks
//! about itself, the lists and the revocation of devices, and the administrator's two switches.
//!
//! - **Three credentials, three mounts.** `GET` and `DELETE /api/device/me` are an [`Access::Device`] part: the router has
//!   already read one bearer token and found an unexpired device of an active account, and has never looked at a cookie. The
//!   start and the end of a grant (`code`, `token`) are an [`Access::DeviceOpen`] part: no credential, JSON only. Everything
//!   a person does (`lookup`, `approve`, `deny`, the lists, the revocation, the switches) is an [`Access::SessionFresh`] part,
//!   so a bearer token cannot reach it and a stale sign-in cannot change anything.
//! - **Behind the gate.** Everything under `/api/device/` is answered as a path that does not exist while both switches are
//!   off (see [`crate::http::features`]). The lists, the revocation and the switch routes are not under a gate: a device must
//!   be revocable whatever the switches say, and the switches are how a feature is turned on.
//! - **The grant is guessed at its own cost.** A device code is 256 bits and a user code is eight of twenty letters; both live
//!   ten minutes and only their hashes are stored. Wrong codes (unknown, expired, malformed) are counted per account and per
//!   address and lock both after five in ten minutes, a poll that comes too soon is told to slow down, open grants are capped
//!   per address, per IPv6 /48 and for the whole server, and an unknown device code is answered as an expired one.
//! - **The token is delivered once.** The poll that finds the grant approved makes the device and its token in one transaction,
//!   deletes the grant and answers with the token; the token string exists nowhere else, and no log, audit line, list or
//!   `Debug` text holds it.
//! - **One way to take a device back.** A person, an administrator and a device that signs out all end a device through the
//!   same function: the row goes, one `device.revoke` line is written and the open sockets of the device are told.
//! - **Nothing beyond what the caller knows.** The answer to a device holds the device's own row and the two switch values; it
//!   names no account. A list holds no hash and no token.
//! - **One audit line per decision.** Approving, denying and revoking name the action, the account and the row (never a code,
//!   a token or a name); a switch that really changed writes `device_access.change` with the switch and the new value; a
//!   request that changes nothing writes nothing.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::Path;
use axum::extract::rejection::PathRejection;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{Action, Actor, DetailKey, DetailValue, Event, Outcome};
use crate::auth::throttle::scoped_key;
use crate::clock::{from_unix, rfc3339};
use crate::device::{
    DeviceToken, GRANT_CAP_RETRY, GRANT_FAILURES, GRANT_TTL, GrantCode, MAX_PENDING, MAX_PENDING_PER_ADDR, MAX_PENDING_PER_NET,
    MAX_POLL_INTERVAL, PLATFORMS, POLL_INTERVAL, PollVerdict, UserCode, poll_verdict,
};
use crate::http::app::App;
use crate::http::features::{Changed, Feature};
use crate::http::router::{Access, Admin, ApiError, ApiJson, Authed, ClientAddr, DeviceAuthed, Part};
use crate::log::{self, Level};
use crate::proxy::key_of;
use crate::store::{AccountId, Device, Grant, NewDevice, NewGrant, StoreError};

use super::routes_auth::{Object, Refusal, answer, blocking};
use super::routes_me::{no_content, present};

/// The most digits of a row id in a path.
const MAX_ID_DIGITS: usize = 18;

/// How often a fresh user code is drawn when the one drawn is already in use. A collision among the open grants is a chance of
/// one in billions; the bound only keeps a broken source of randomness from looping.
const CODE_DRAWS: usize = 4;

fn bad(field: &'static str) -> Refusal {
    Refusal(ApiError::BadRequest { field: Some(field) })
}

fn busy() -> Refusal {
    Refusal(ApiError::Busy)
}

fn time_text(unix: i64) -> String {
    rfc3339(from_unix(unix))
}

/// Room for the whole answer that hands a device its token: the members and their names, the 47 characters of the token, a
/// row id and a time come to well under this.
const APPROVED_BYTES: usize = 512;

/// The answer that hands a device its token. The text is written once, straight into the buffer that becomes the body, which
/// is large enough from the start so that no outgrown buffer with a part of the token is left behind, and no other copy of the
/// token is made (no JSON value, no second serialisation). The body itself goes to the connection and cannot be wiped; the
/// token's own text is wiped when it is dropped.
fn approved_answer(token: &DeviceToken, device: &Device) -> Result<Response, Refusal> {
    #[derive(Serialize)]
    struct Approved<'a> {
        state: &'static str,
        token: &'a str,
        device_id: i64,
        expires_at: String,
    }
    let approved =
        Approved { state: "approved", token: token.expose(), device_id: device.id, expires_at: time_text(device.expires_at) };
    let mut body = Vec::with_capacity(APPROVED_BYTES);
    serde_json::to_writer(&mut body, &approved).map_err(|_| busy())?;
    let mut response = (StatusCode::OK, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// The row id a path names: plain digits, at most [`MAX_ID_DIGITS`] of them. Anything else names no device.
fn row_id(path: Result<Path<String>, PathRejection>) -> Option<i64> {
    let Path(text) = path.ok()?;
    if text.is_empty() || text.len() > MAX_ID_DIGITS || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn record(app: &App, event: &Event, what: &'static str) {
    if app.audit.record(event).is_err() {
        log::event(Level::Error, what, &[]);
    }
}

/// `GET /api/device/me`: the device that sent the token, and which features are on for it.
async fn me_route(Extension(app): Extension<Arc<App>>, who: DeviceAuthed) -> Response {
    let effective = app.features.effective();
    answer(
        StatusCode::OK,
        &json!({
            "device": {
                "id": who.device.id,
                "name": who.device.name,
                "platform": who.device.platform,
                "expires_at": rfc3339(from_unix(who.device.expires_at)),
            },
            "switches": { "sync": effective.sync, "remote": effective.remote },
        }),
        Vec::new(),
    )
}

/// The state of the two switches as the administrator sees it: what is stored, and whether the configuration allows it.
fn state_of(app: &App) -> Value {
    let one = |feature: Feature| json!({ "on": app.features.stored(feature), "allowed": app.features.ceiling().allows(feature) });
    json!({ "sync": one(Feature::Sync), "remote": one(Feature::Remote) })
}

/// `GET /api/admin/device-access`.
async fn access_get_route(Extension(app): Extension<Arc<App>>, Admin(_who): Admin) -> Response {
    answer(StatusCode::OK, &state_of(&app), Vec::new())
}

/// The body of a change: one or both switches, each a boolean. Each is any JSON value here, so that a wrong type names the
/// member and changes nothing.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessBody {
    #[serde(default, deserialize_with = "present")]
    sync: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    remote: Option<Value>,
}

fn flag(value: Option<Value>, field: &'static str) -> Result<Option<bool>, Refusal> {
    match value {
        None => Ok(None),
        Some(Value::Bool(on)) => Ok(Some(on)),
        Some(_) => Err(Refusal(ApiError::BadRequest { field: Some(field) })),
    }
}

/// `POST /api/admin/device-access`: turns a switch on or off. Turning one on that the configuration does not allow is 409 on
/// that member and changes nothing, whatever else the body says. The answer is the new state.
async fn access_post_route(
    Extension(app): Extension<Arc<App>>,
    Admin(who): Admin,
    body: Result<ApiJson<Object<AccessBody>>, ApiError>,
) -> Result<Response, Refusal> {
    let ApiJson(Object(body)) = body?;
    let sync = flag(body.sync, "sync")?;
    let remote = flag(body.remote, "remote")?;
    if sync.is_none() && remote.is_none() {
        return Err(Refusal(ApiError::BadRequest { field: None }));
    }
    for (feature, wanted) in [(Feature::Sync, sync), (Feature::Remote, remote)] {
        if wanted == Some(true) && !app.features.ceiling().allows(feature) {
            return Err(Refusal(ApiError::Conflict { field: Some(feature.key()) }));
        }
    }
    let (actor, addr) = (who.account.id, Some(who.client));
    let worker = app.clone();
    let changed: Changed = blocking(move || worker.features.apply(sync, remote))
        .await?
        .map_err(|_| Refusal(ApiError::Busy))?;
    for (feature, on) in changed.list() {
        let event = Event::new(Action::DeviceAccessChange, Outcome::Ok, Actor::Account(actor), addr)
            .detail(DetailKey::Switch, DetailValue::Text(feature.key()))
            .detail(DetailKey::State, DetailValue::Text(if on { "on" } else { "off" }));
        if app.audit.record(&event).is_err() {
            log::event(Level::Error, "device_access.audit_failed", &[]);
        }
    }
    Ok(answer(StatusCode::OK, &state_of(&app), Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// The throttle of wrong codes
// ---------------------------------------------------------------------------------------------------------------------

/// The keys the wrong codes of a signed-in person are counted under: the account and the address. The scopes are ones the
/// sign-in does not use, so the two sets of counts never meet.
fn person_keys(account: AccountId, client: IpAddr) -> [String; 2] {
    [scoped_key("dp", account.get().to_string().as_bytes()), scoped_key("dpi", key_of(client).to_string().as_bytes())]
}

/// The key the unknown device codes of an anonymous device are counted under: its address.
fn poll_keys(client: IpAddr) -> [String; 1] {
    [scoped_key("dd", key_of(client).to_string().as_bytes())]
}

/// The seconds left of the longest lock among `keys`, or `None` when none is locked. Only reads.
fn locked_for(app: &App, keys: &[String], now: i64) -> Result<Option<u64>, ApiError> {
    let until = app
        .store
        .read(|tx| {
            let mut until = 0;
            for key in keys {
                let state = tx.throttle_get(key, now, GRANT_FAILURES.window_secs)?;
                if state.is_locked(now) {
                    until = until.max(state.locked_until);
                }
            }
            Ok(until)
        })
        .map_err(|_| ApiError::Busy)?;
    Ok((until > now).then(|| u64::try_from(until - now).unwrap_or(1).max(1)))
}

/// Counts one wrong code under every key, in one transaction. A store that cannot count is logged and the wrong code is still
/// refused.
fn count_failure(app: &App, keys: &[String], now: i64) {
    let counted = app.store.write(|tx| {
        for key in keys {
            tx.throttle_fail(key, now, &GRANT_FAILURES)?;
        }
        Ok(())
    });
    if counted.is_err() {
        log::event(Level::Error, "device_grant.throttle_failed", &[]);
    }
}

fn secs(duration: std::time::Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/device/code
// ---------------------------------------------------------------------------------------------------------------------

/// The body of the start of a grant. Each member is any JSON value here, so that a member of the wrong type names itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    #[serde(default, deserialize_with = "present")]
    name: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    platform: Option<Value>,
}

/// What the start of a grant came to.
enum Started {
    /// A cap on open grants is reached.
    Full,
    /// The grant was made.
    Made(GrantCode, UserCode),
}

/// `POST /api/device/code`: an app with no credential asks for a grant. The answer holds the device code only this caller will
/// ever see, the user code to show the person, and the address to open: the server's own, built from its configuration, with
/// the user code in the fragment so that it never reaches a server log.
async fn code_route(
    Extension(app): Extension<Arc<App>>,
    Extension(ClientAddr(client)): Extension<ClientAddr>,
    ApiJson(Object(body)): ApiJson<Object<StartBody>>,
) -> Result<Response, Refusal> {
    let Some(Value::String(name)) = body.name else {
        return Err(bad("name"));
    };
    let platform = match body.platform {
        Some(Value::String(platform)) if PLATFORMS.contains(&platform.as_str()) => platform,
        _ => return Err(bad("platform")),
    };
    let worker = app.clone();
    // The address is counted as the throttle counts it (an IPv6 address by its /64), so that one subscriber cannot open a grant
    // from every address of its own network. It is also the address the person is shown.
    let counted = key_of(client);
    let made = blocking(move || {
        let now = worker.clock.unix();
        worker.store.write(|tx| {
            let open = tx.grant_counts(counted, now)?;
            if open.all >= MAX_PENDING || open.from_addr >= MAX_PENDING_PER_ADDR || open.from_net >= MAX_PENDING_PER_NET {
                return Ok(Started::Full);
            }
            let device_code = GrantCode::generate().map_err(|_| StoreError::Storage("no random bytes"))?;
            for _ in 0..CODE_DRAWS {
                let user_code = UserCode::generate().map_err(|_| StoreError::Storage("no random bytes"))?;
                let new = NewGrant {
                    device_code_hash: device_code.hash(),
                    user_code_hash: user_code.hash(),
                    name: &name,
                    platform: &platform,
                    now,
                    expires_at: now.saturating_add(GRANT_TTL),
                    addr: counted,
                };
                match tx.grant_create(&new) {
                    Ok(_) => return Ok(Started::Made(device_code, user_code)),
                    Err(StoreError::Conflict) => {}
                    Err(other) => return Err(other),
                }
            }
            Err(StoreError::Storage("no free user code"))
        })
    })
    .await?;
    let made = match made {
        Ok(made) => made,
        Err(StoreError::Invalid(_)) => return Err(bad("name")),
        Err(_) => return Err(busy()),
    };
    let Started::Made(device_code, user_code) = made else {
        return Err(Refusal(ApiError::Throttled { retry_seconds: GRANT_CAP_RETRY }));
    };
    let uri = format!("{}/", app.origin.text());
    let shown = user_code.display();
    Ok(answer(
        StatusCode::OK,
        &json!({
            "device_code": device_code.expose(),
            "user_code": shown,
            "verification_uri": uri,
            "verification_uri_complete": format!("{uri}#device={shown}"),
            "expires_in": GRANT_TTL,
            "interval": POLL_INTERVAL,
        }),
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/device/token
// ---------------------------------------------------------------------------------------------------------------------

/// The body of a poll: the device code.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PollBody {
    #[serde(default, deserialize_with = "present")]
    device_code: Option<Value>,
}

/// What a poll came to.
enum Polled {
    /// No grant has this code.
    Unknown,
    /// A state word, with the interval the device has to keep when the state goes on.
    State(&'static str, Option<i64>),
    /// The device was made; the token is in this answer and nowhere else.
    Approved(Box<(DeviceToken, Device)>),
}

/// One poll, in one transaction: the verdict by arithmetic on the stored times, the poll noted, and on approval the device and its
/// token made and the grant deleted. The 21st device of an account is not made and the grant stays pending.
fn poll_once(app: &App, client: IpAddr, hash: &crate::store::TokenHash, now: i64) -> Result<Polled, StoreError> {
    let idle = secs(app.devices.idle);
    app.store.write(|tx| {
        let Some(grant) = tx.grant_by_device_code(hash)? else {
            return Ok(Polled::Unknown);
        };
        let verdict = poll_verdict(&grant, now);
        match verdict {
            PollVerdict::Expired => Ok(Polled::State("expired", None)),
            PollVerdict::SlowDown => {
                tx.grant_poll_mark(grant.id, now, true)?;
                Ok(Polled::State(
                    "slow_down",
                    Some(grant.poll_interval.saturating_add(POLL_INTERVAL).min(MAX_POLL_INTERVAL)),
                ))
            }
            PollVerdict::Denied => {
                tx.grant_poll_mark(grant.id, now, false)?;
                Ok(Polled::State("denied", None))
            }
            PollVerdict::Pending => {
                tx.grant_poll_mark(grant.id, now, false)?;
                Ok(Polled::State("pending", Some(grant.poll_interval)))
            }
            PollVerdict::Approved => {
                let Some(account) = grant.approved_by else {
                    return Ok(Polled::State("pending", Some(grant.poll_interval)));
                };
                if tx.devices_of(account, now)?.len() >= crate::device::MAX_DEVICES_PER_ACCOUNT {
                    tx.grant_poll_mark(grant.id, now, false)?;
                    return Ok(Polled::State("too_many", Some(grant.poll_interval)));
                }
                let Some(taken) = tx.grant_take(hash, now)? else {
                    return Ok(Polled::State("expired", None));
                };
                let token = DeviceToken::generate().map_err(|_| StoreError::Storage("no random bytes"))?;
                let device = tx.device_create(&NewDevice {
                    token_hash: token.hash(),
                    account_id: account,
                    name: &taken.name,
                    platform: &taken.platform,
                    now,
                    expires_at: now.saturating_add(idle),
                    addr: client,
                })?;
                Ok(Polled::Approved(Box::new((token, device))))
            }
        }
    })
}

/// `POST /api/device/token`: the app asks whether its grant was decided. The answer is always 200 with a `state`: `pending`,
/// `slow_down`, `denied`, `expired` (also for a code that never existed, so the two cannot be told apart), `too_many` (the
/// account already has the most devices; the grant waits) or `approved`, which carries the token, the device id and the expiry
/// once. A client address that sends unknown codes five times in ten minutes is refused with 429 until the window passes.
async fn token_route(
    Extension(app): Extension<Arc<App>>,
    Extension(ClientAddr(client)): Extension<ClientAddr>,
    ApiJson(Object(body)): ApiJson<Object<PollBody>>,
) -> Result<Response, Refusal> {
    let Some(Value::String(text)) = body.device_code else {
        return Err(bad("device_code"));
    };
    let worker = app.clone();
    let polled = blocking(move || -> Result<Polled, ApiError> {
        let now = worker.clock.unix();
        let keys = poll_keys(client);
        if let Some(retry_seconds) = locked_for(&worker, &keys, now)? {
            return Err(ApiError::Throttled { retry_seconds });
        }
        let Ok(code) = GrantCode::parse(&text) else {
            count_failure(&worker, &keys, now);
            return Ok(Polled::State("expired", None));
        };
        let hash = code.hash();
        drop(code);
        match poll_once(&worker, client, &hash, now).map_err(|_| ApiError::Busy)? {
            Polled::Unknown => {
                count_failure(&worker, &keys, now);
                Ok(Polled::State("expired", None))
            }
            other => Ok(other),
        }
    })
    .await?
    .map_err(Refusal)?;
    Ok(match polled {
        Polled::State(state, Some(interval)) => {
            answer(StatusCode::OK, &json!({ "state": state, "interval": interval }), Vec::new())
        }
        Polled::State(state, None) => answer(StatusCode::OK, &json!({ "state": state }), Vec::new()),
        Polled::Approved(delivered) => {
            let (token, device) = *delivered;
            approved_answer(&token, &device)?
        }
        Polled::Unknown => answer(StatusCode::OK, &json!({ "state": "expired" }), Vec::new()),
    })
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/device/lookup, /approve and /deny
// ---------------------------------------------------------------------------------------------------------------------

/// The body of a lookup, an approval and a refusal: the user code as the person has it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    #[serde(default, deserialize_with = "present")]
    code: Option<Value>,
}

/// The open grant a user code names. A code that is malformed, unknown or expired counts as a wrong code under the account and
/// the address, and every further try is refused for a while once five were wrong; a code of a grant that was already decided
/// is as gone as the rest but is not counted, because a person who presses twice is not guessing. All of them answer `gone`.
fn open_grant(app: &App, account: AccountId, client: IpAddr, text: &str) -> Result<Grant, ApiError> {
    let now = app.clock.unix();
    let keys = person_keys(account, client);
    if let Some(retry_seconds) = locked_for(app, &keys, now)? {
        return Err(ApiError::Throttled { retry_seconds });
    }
    let found = match UserCode::normalise(text) {
        Some(code) => app.store.read(|tx| tx.grant_by_user_code(&code.hash())).map_err(|_| ApiError::Busy)?,
        None => None,
    };
    match found {
        Some(grant) if now < grant.expires_at => {
            if grant.approved_by.is_some() || grant.denied {
                Err(ApiError::Gone)
            } else {
                Ok(grant)
            }
        }
        _ => {
            count_failure(app, &keys, now);
            Err(ApiError::Gone)
        }
    }
}

fn code_text(body: CodeBody) -> Result<String, Refusal> {
    match body.code {
        Some(Value::String(text)) => Ok(text),
        _ => Err(bad("code")),
    }
}

/// `POST /api/device/lookup`: who asks. The answer holds what the person is asked to approve and nothing of the codes.
async fn lookup_route(
    Extension(app): Extension<Arc<App>>,
    who: Authed,
    ApiJson(Object(body)): ApiJson<Object<CodeBody>>,
) -> Result<Response, Refusal> {
    let text = code_text(body)?;
    let (account, client) = (who.account.id, who.client);
    let grant = blocking(move || open_grant(&app, account, client, &text)).await?.map_err(Refusal)?;
    Ok(answer(
        StatusCode::OK,
        &json!({
            "name": grant.name,
            "platform": grant.platform,
            "addr": grant.addr,
            "created_at": time_text(grant.created_at),
        }),
        Vec::new(),
    ))
}

/// `POST /api/device/approve`: the signed-in person (with a recent sign-in, which the mount checked) approves the grant; the next
/// poll of the app gets its token. A grant that was decided or ran out in between is `gone`.
async fn approve_route(
    Extension(app): Extension<Arc<App>>,
    who: Authed,
    ApiJson(Object(body)): ApiJson<Object<CodeBody>>,
) -> Result<Response, Refusal> {
    let text = code_text(body)?;
    let (account, client) = (who.account.id, who.client);
    let worker = app.clone();
    let grant = blocking(move || -> Result<Grant, ApiError> {
        let grant = open_grant(&worker, account, client, &text)?;
        let now = worker.clock.unix();
        let approved = worker.store.write(|tx| tx.grant_approve(grant.id, account, now)).map_err(|_| ApiError::Busy)?;
        if approved { Ok(grant) } else { Err(ApiError::Gone) }
    })
    .await?
    .map_err(Refusal)?;
    let event = Event::new(Action::DeviceApprove, Outcome::Ok, Actor::Account(account), Some(client))
        .detail(DetailKey::Grant, DetailValue::Number(grant.id));
    record(&app, &event, "device_grant.audit_failed");
    Ok(no_content(Vec::new()))
}

/// `POST /api/device/deny`: the signed-in person refuses the grant; the next poll of the app gets `denied` and no device exists.
async fn deny_route(
    Extension(app): Extension<Arc<App>>,
    who: Authed,
    ApiJson(Object(body)): ApiJson<Object<CodeBody>>,
) -> Result<Response, Refusal> {
    let text = code_text(body)?;
    let (account, client) = (who.account.id, who.client);
    let worker = app.clone();
    let grant = blocking(move || -> Result<Grant, ApiError> {
        let grant = open_grant(&worker, account, client, &text)?;
        let now = worker.clock.unix();
        let denied = worker.store.write(|tx| tx.grant_deny(grant.id, now)).map_err(|_| ApiError::Busy)?;
        if denied { Ok(grant) } else { Err(ApiError::Gone) }
    })
    .await?
    .map_err(Refusal)?;
    let event = Event::new(Action::DeviceDeny, Outcome::Ok, Actor::Account(account), Some(client))
        .detail(DetailKey::Grant, DetailValue::Number(grant.id));
    record(&app, &event, "device_grant.audit_failed");
    Ok(no_content(Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// The lists and the revocation
// ---------------------------------------------------------------------------------------------------------------------

/// A device as a list shows it: its row and its times, and no hash and no token.
fn device_view(device: &Device) -> Value {
    json!({
        "id": device.id,
        "name": device.name,
        "platform": device.platform,
        "created_at": time_text(device.created_at),
        "last_seen": time_text(device.last_seen),
        "expires_at": time_text(device.expires_at),
    })
}

/// `GET /api/me/devices`: the devices of the signed-in person, newest first. Another person's device never appears.
async fn me_list_route(Extension(app): Extension<Arc<App>>, who: Authed) -> Result<Response, Refusal> {
    let account = who.account.id;
    let listed =
        blocking(move || app.store.read(|tx| tx.devices_of(account, app.clock.unix()))).await?.map_err(|_| busy())?;
    let devices: Vec<Value> = listed.iter().map(device_view).collect();
    Ok(answer(StatusCode::OK, &json!({ "devices": devices }), Vec::new()))
}

/// `GET /api/admin/devices`: every device of every person, newest first, each with its owner's user name and display name.
async fn admin_list_route(Extension(app): Extension<Arc<App>>, Admin(_who): Admin) -> Result<Response, Refusal> {
    let listed = blocking(move || {
        app.store.read(|tx| {
            let mut rows = Vec::new();
            for device in tx.devices_all(app.clock.unix())? {
                let owner = tx.account_get(device.account_id)?;
                let mut view = device_view(&device);
                if let Some(object) = view.as_object_mut() {
                    object.insert("username".to_owned(), json!(owner.as_ref().map_or("", |a| a.username.as_str())));
                    object.insert(
                        "display_name".to_owned(),
                        json!(owner.as_ref().map_or("", |a| a.display_name.as_str())),
                    );
                }
                rows.push(view);
            }
            Ok(rows)
        })
    })
    .await?
    .map_err(|_| busy())?;
    Ok(answer(StatusCode::OK, &json!({ "devices": listed }), Vec::new()))
}

/// Which device a revocation names, and on whose authority.
enum Revoke {
    /// A device of this account only: what a person, or the device itself, may end.
    Own { account: AccountId, id: i64 },
    /// Any device: what an administrator may end.
    Any { id: i64 },
}

/// Ends a device. This is the one place that does it, for a person, an administrator and the device itself: the row is deleted,
/// one `device.revoke` line names the actor, the owner and the row, and the open sockets of the device are told. Answers whether
/// there was such a device (an id that is not the caller's is not one).
fn revoke(app: &App, which: &Revoke, actor: AccountId, client: IpAddr) -> Result<bool, ApiError> {
    let (id, owner) = match *which {
        Revoke::Own { account, id } => {
            let gone = app.store.write(|tx| tx.device_revoke(account, id)).map_err(|_| ApiError::Busy)?;
            (id, gone.then_some(account))
        }
        Revoke::Any { id } => (id, app.store.write(|tx| tx.device_revoke_any(id)).map_err(|_| ApiError::Busy)?),
    };
    let Some(owner) = owner else { return Ok(false) };
    let event = Event::new(Action::DeviceRevoke, Outcome::Ok, Actor::Account(actor), Some(client))
        .target(owner)
        .detail(DetailKey::Device, DetailValue::Number(id));
    record(app, &event, "device_revoke.audit_failed");
    app.session_manager.announce_device(id);
    Ok(true)
}

/// `DELETE /api/me/devices/{id}`: the signed-in person ends one of their own devices. Its token is refused from the next request
/// on. An id that is not theirs, or is not a number, is 404.
async fn me_revoke_route(
    Extension(app): Extension<Arc<App>>,
    who: Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let Some(id) = row_id(path) else {
        return Err(Refusal(ApiError::NotFound));
    };
    let (account, client) = (who.account.id, who.client);
    let ended =
        blocking(move || revoke(&app, &Revoke::Own { account, id }, account, client)).await?.map_err(Refusal)?;
    if ended { Ok(no_content(Vec::new())) } else { Err(Refusal(ApiError::NotFound)) }
}

/// `DELETE /api/admin/devices/{id}`: an administrator ends any device.
async fn admin_revoke_route(
    Extension(app): Extension<Arc<App>>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let Some(id) = row_id(path) else {
        return Err(Refusal(ApiError::NotFound));
    };
    let (actor, client) = (who.account.id, who.client);
    let ended = blocking(move || revoke(&app, &Revoke::Any { id }, actor, client)).await?.map_err(Refusal)?;
    if ended { Ok(no_content(Vec::new())) } else { Err(Refusal(ApiError::NotFound)) }
}

/// `DELETE /api/device/me`: the device ends its own sign-in (what Sign out of the app calls). The same function as every other
/// revocation does it; the token is refused from the next request on.
async fn me_end_route(Extension(app): Extension<Arc<App>>, who: DeviceAuthed) -> Result<Response, Refusal> {
    let (account, id, client) = (who.account.id, who.device.id, who.client);
    let ended =
        blocking(move || revoke(&app, &Revoke::Own { account, id }, account, client)).await?.map_err(Refusal)?;
    if ended { Ok(no_content(Vec::new())) } else { Err(Refusal(ApiError::Invalid)) }
}

/// The routes of the device features, as parts for [`crate::http::router::build`].
pub fn parts() -> Vec<Part> {
    let device = Router::new().route("/api/device/me", get(me_route).delete(me_end_route));
    let open = Router::new().route("/api/device/code", post(code_route)).route("/api/device/token", post(token_route));
    let person = Router::new()
        .route("/api/device/lookup", post(lookup_route))
        .route("/api/device/approve", post(approve_route))
        .route("/api/device/deny", post(deny_route))
        .route("/api/me/devices", get(me_list_route))
        .route("/api/me/devices/{id}", delete(me_revoke_route));
    let admin = Router::new()
        .route("/api/admin/device-access", get(access_get_route).post(access_post_route))
        .route("/api/admin/devices", get(admin_list_route))
        .route("/api/admin/devices/{id}", delete(admin_revoke_route));
    vec![
        Part { access: Access::Device, router: device },
        Part { access: Access::DeviceOpen, router: open },
        Part { access: Access::SessionFresh, router: person },
        Part { access: Access::SessionFresh, router: admin },
    ]
}
