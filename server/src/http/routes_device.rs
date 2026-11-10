//! The routes of the device features: what a device asks about itself under `/api/device`.
//!
//! - **The Device mount.** `GET /api/device/me` is an [`Access::Device`] part: the router has already read one bearer token and
//!   found an unexpired device of an active account, and has never looked at a cookie.
//! - **Behind the gate.** Everything under `/api/device/` is answered as a path that does not exist while both switches are
//!   off (see [`crate::http::features`]).
//! - **Nothing beyond what the caller knows.** The answer to a device holds the device's own row and the two switch values; it
//!   names no account.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use serde_json::json;

use crate::clock::{from_unix, rfc3339};
use crate::http::app::App;
use crate::http::router::{Access, DeviceAuthed, Part};

use super::routes_auth::answer;

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

/// The routes of the device features, as parts for [`crate::http::router::build`].
pub fn parts() -> Vec<Part> {
    let device = Router::new().route("/api/device/me", get(me_route));
    vec![Part { access: Access::Device, router: device }]
}
