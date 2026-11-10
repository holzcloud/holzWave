//! The routes of the device features: what a device asks about itself under `/api/device`, and the administrator's two
//! switches under `/api/admin/device-access`.
//!
//! - **Two mounts, two credentials.** `GET /api/device/me` is an [`Access::Device`] part: the router has already read one bearer
//!   token and found an unexpired device of an active account, and has never looked at a cookie. The switch routes are an
//!   [`Access::SessionFresh`] part, so a bearer token cannot reach them and a stale sign-in cannot change a switch.
//! - **Behind the gate.** Everything under `/api/device/` is answered as a path that does not exist while both switches are
//!   off (see [`crate::http::features`]); the switch routes are not under a gate, because they are how a switch is turned on.
//! - **Nothing beyond what the caller knows.** The answer to a device holds the device's own row and the two switch values; it
//!   names no account.
//! - **One audit line per change.** A switch that really changed writes `device_access.change` with the switch and the new
//!   value; a request that changes nothing writes nothing.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::audit::{Action, Actor, DetailKey, DetailValue, Event, Outcome};
use crate::clock::{from_unix, rfc3339};
use crate::http::app::App;
use crate::http::features::{Changed, Feature};
use crate::http::router::{Access, Admin, ApiError, ApiJson, DeviceAuthed, Part};
use crate::log::{self, Level};

use super::routes_auth::{Object, Refusal, answer, blocking};
use super::routes_me::present;

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

/// The routes of the device features, as parts for [`crate::http::router::build`].
pub fn parts() -> Vec<Part> {
    let device = Router::new().route("/api/device/me", get(me_route));
    let admin = Router::new().route("/api/admin/device-access", get(access_get_route).post(access_post_route));
    vec![Part { access: Access::Device, router: device }, Part { access: Access::SessionFresh, router: admin }]
}
