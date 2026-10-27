//! The cookies of the server: their names, the strict reader of the session cookie and (below) the builders.
//!
//! Every cookie name carries the `__Host-` prefix, so a browser accepts it only over TLS, with `Path=/` and without a
//! `Domain`. The session cookie is read once per request from the `Cookie` header with hard bounds; whatever is not exactly
//! what the server wrote counts as no cookie.

use axum::http::header::{self, HeaderMap, HeaderValue};
use cookie::Cookie;

use crate::auth::session::TOKEN_CHARS;

/// The session cookie.
pub const SESSION_COOKIE: &str = "__Host-hw_session";

/// The cookie of an OpenID Connect sign-in in progress.
pub const OIDC_COOKIE: &str = "__Host-hw_oidc";

/// The cookie of a sign-in that has more than one step.
pub const PENDING_COOKIE: &str = "__Host-hw_pending";

/// The most bytes of the `Cookie` header (all lines together) that are read.
pub const MAX_COOKIE_HEADER_BYTES: usize = 8 * 1024;

/// The most name and value pairs that are read.
pub const MAX_COOKIE_PAIRS: usize = 64;

/// Whether `value` has the shape of a session value: exactly 43 characters of the base64url alphabet.
pub fn is_session_value(value: &str) -> bool {
    value.len() == TOKEN_CHARS && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The value of the session cookie of a request, or `None`.
///
/// `None` also for: a header (all lines together) over [`MAX_COOKIE_HEADER_BYTES`], more than [`MAX_COOKIE_PAIRS`] pairs, a
/// pair that does not parse, the session cookie name twice (a cookie from a looser context must not win), and a value that
/// is not exactly 43 characters of the base64url alphabet (so a quoted value, a value with a space and a value of another
/// length are all refused here, before anything else looks at them).
pub fn read_session(headers: &HeaderMap) -> Option<String> {
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
            if cookie.name() == SESSION_COOKIE {
                if found.is_some() {
                    return None;
                }
                found = Some(cookie.value().to_owned());
            }
        }
    }
    found.filter(|value| is_session_value(value))
}

/// The `Max-Age` of the OpenID Connect transaction cookie, in seconds.
pub const OIDC_MAX_AGE: u32 = 600;

/// The `Max-Age` of the pending-step cookie, in seconds.
pub const PENDING_MAX_AGE: u32 = 300;

/// The longest value of the OIDC and pending cookies.
pub const MAX_TOKEN_CHARS: usize = 256;

/// Whether `value` is safe to write into a cookie: 1 to [`MAX_TOKEN_CHARS`] characters of the base64url alphabet and `.`/`~`.
fn is_cookie_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_CHARS
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}

fn header_value(text: String) -> HeaderValue {
    // Every part of `text` is an ASCII constant or a value that was checked, so this cannot fail.
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// A header value that holds a secret: its `Debug` text is the word `Sensitive`, and HTTP/2 does not index it.
fn secret_header_value(text: String) -> HeaderValue {
    let mut value = header_value(text);
    value.set_sensitive(true);
    value
}

/// The `Set-Cookie` value that sets the session cookie: `__Host-hw_session=<v>; Secure; HttpOnly; SameSite=Strict; Path=/`.
/// `None` for a value that is not 43 characters of the base64url alphabet. No `Domain`, no `Max-Age`: the session ends when
/// the server says so.
pub fn session(value: &str) -> Option<HeaderValue> {
    is_session_value(value).then(|| secret_header_value(format!("{SESSION_COOKIE}={value}; Secure; HttpOnly; SameSite=Strict; Path=/")))
}

/// The `Set-Cookie` value that deletes the session cookie.
pub fn delete_session() -> HeaderValue {
    header_value(format!("{SESSION_COOKIE}=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))
}

/// The `Set-Cookie` value of the OpenID Connect transaction cookie: `Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=600`.
/// It is `Lax` because the provider sends the browser back with a top-level navigation from another site.
pub fn oidc(value: &str) -> Option<HeaderValue> {
    is_cookie_token(value)
        .then(|| secret_header_value(format!("{OIDC_COOKIE}={value}; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age={OIDC_MAX_AGE}")))
}

/// The `Set-Cookie` value that deletes the OpenID Connect transaction cookie.
pub fn delete_oidc() -> HeaderValue {
    header_value(format!("{OIDC_COOKIE}=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"))
}

/// The `Set-Cookie` value of the pending-step cookie: `Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=300`.
pub fn pending(value: &str) -> Option<HeaderValue> {
    is_cookie_token(value).then(|| {
        secret_header_value(format!("{PENDING_COOKIE}={value}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age={PENDING_MAX_AGE}"))
    })
}

/// The `Set-Cookie` value that deletes the pending-step cookie.
pub fn delete_pending() -> HeaderValue {
    header_value(format!("{PENDING_COOKIE}=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"))
}
