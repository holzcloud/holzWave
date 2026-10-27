//! The headers every response carries, whatever route made it and however it ended.
//!
//! The policy is the one of the page contract: nothing but the page's own origin, plus the secure WebSocket to the public
//! host. There is no `unsafe-inline`, no `unsafe-eval`, no wildcard and no foreign host. The server sends no cross-origin
//! sharing header of any kind; a route that adds one has it removed here.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

use super::app::PublicOrigin;

/// One year, in seconds. No sub-domains and no preload: the operator may own other names under the domain.
pub const HSTS: &str = "max-age=31536000";

/// What the browser may do with its device APIs: the clipboard and the credential APIs for the page itself, nothing else.
pub const PERMISSIONS_POLICY: &str = "accelerometer=(), bluetooth=(), camera=(), display-capture=(), geolocation=(), gyroscope=(), hid=(), \
magnetometer=(), microphone=(), midi=(), payment=(), serial=(), usb=(), xr-spatial-tracking=(), \
clipboard-read=(self), clipboard-write=(self), publickey-credentials-create=(self), publickey-credentials-get=(self)";

/// The content security policy for a page served from `authority` (the host, with the port unless it is 443).
pub fn content_security_policy(authority: &str) -> String {
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; media-src 'self'; font-src 'self'; \
connect-src 'self' wss://{authority}; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'"
    )
}

const CSP: HeaderName = HeaderName::from_static("content-security-policy");
const HSTS_NAME: HeaderName = HeaderName::from_static("strict-transport-security");
const REFERRER: HeaderName = HeaderName::from_static("referrer-policy");
const SNIFF: HeaderName = HeaderName::from_static("x-content-type-options");
const COOP: HeaderName = HeaderName::from_static("cross-origin-opener-policy");
const CORP: HeaderName = HeaderName::from_static("cross-origin-resource-policy");
const PERMISSIONS: HeaderName = HeaderName::from_static("permissions-policy");

/// The fixed headers of one server, made once from the public address.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    csp: HeaderValue,
}

impl SecurityHeaders {
    /// The headers for a server at `origin`.
    pub fn new(origin: &PublicOrigin) -> SecurityHeaders {
        // The authority is checked by the configuration (letters, digits, hyphens, dots, a port, brackets), so it is a legal
        // header value; a value that is not would be a defect, and the strictest policy is used then.
        let csp = HeaderValue::from_str(&content_security_policy(origin.authority()))
            .unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"));
        SecurityHeaders { csp }
    }

    /// Puts the fixed headers on `headers`, replacing any value a route set, removes every cross-origin sharing header, and
    /// adds `Cache-Control: no-store` unless a route chose its own.
    pub fn apply(&self, headers: &mut HeaderMap) {
        let doomed: Vec<HeaderName> =
            headers.keys().filter(|name| name.as_str().starts_with("access-control-")).cloned().collect();
        for name in doomed {
            headers.remove(name);
        }
        headers.insert(CSP, self.csp.clone());
        headers.insert(HSTS_NAME, HeaderValue::from_static(HSTS));
        headers.insert(REFERRER, HeaderValue::from_static("no-referrer"));
        headers.insert(SNIFF, HeaderValue::from_static("nosniff"));
        headers.insert(COOP, HeaderValue::from_static("same-origin"));
        headers.insert(CORP, HeaderValue::from_static("same-origin"));
        headers.insert(PERMISSIONS, HeaderValue::from_static(PERMISSIONS_POLICY));
        if !headers.contains_key(header::CACHE_CONTROL) {
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        }
    }
}

/// The outermost layer: whatever happened inside, the response carries the fixed headers.
pub async fn layer(State(fixed): State<Arc<SecurityHeaders>>, request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    fixed.apply(response.headers_mut());
    response
}
