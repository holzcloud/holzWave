//! The rules that decide whether a request may change anything: the Host, the Origin, the content type and the anti-forgery
//! token, as pure functions over the headers and the public origin.
//!
//! All comparisons are exact. A name is compared whole (no prefix, no suffix, no path), a repeated header is a refusal, and
//! nothing here reads a forwarded header.

use axum::http::header::{self, HeaderMap};
use axum::http::{Method, Uri};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::keyfile::KEY_BYTES;
use crate::store::TokenHash;

use super::app::PublicOrigin;

/// The name of the header that carries the anti-forgery token.
pub const TOKEN_HEADER: &str = "x-csrf-token";

/// Whether the method changes something: `POST`, `PUT`, `PATCH` and `DELETE`.
pub fn is_state_changing(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE)
}

/// The body a route takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    /// `application/json`, parameters such as the charset allowed.
    Json,
    /// `application/octet-stream`: a file.
    Octet,
}

impl BodyKind {
    fn media_type(self) -> &'static str {
        match self {
            BodyKind::Json => "application/json",
            BodyKind::Octet => "application/octet-stream",
        }
    }
}

/// Whether the request came to the public name: the `Host` header, and the authority of the request target when it has one
/// (an HTTP/2 request has no `Host` header), each equal to the public authority. Two `Host` headers, a missing name, user
/// information, a path and a port that is not the public one are all refused. The comparison ignores case and treats a
/// default port written out as the same as one left out.
pub fn host_ok(origin: &PublicOrigin, headers: &HeaderMap, uri: &Uri) -> bool {
    let mut hosts = headers.get_all(header::HOST).iter();
    let named = match (hosts.next(), hosts.next()) {
        (None, _) => None,
        (Some(one), None) => Some(one),
        (Some(_), Some(_)) => return false,
    };
    let target = uri.authority();
    if named.is_none() && target.is_none() {
        return false;
    }
    if let Some(value) = named {
        match value.to_str() {
            Ok(text) if authority_matches(origin, text) => {}
            _ => return false,
        }
    }
    if let Some(authority) = target
        && !authority_matches(origin, authority.as_str())
    {
        return false;
    }
    true
}

/// Whether `text` is exactly the public authority: `host` or `host:port` (or `[v6]` with or without a port), nothing else.
fn authority_matches(origin: &PublicOrigin, text: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']');
    if text.is_empty() || !text.bytes().all(allowed) {
        return false;
    }
    let (host, port) = match text.strip_prefix('[') {
        Some(inner) => {
            let Some((host, after)) = inner.split_once(']') else { return false };
            match after {
                "" => (host, None),
                _ => match after.strip_prefix(':') {
                    Some(port) => (host, Some(port)),
                    None => return false,
                },
            }
        }
        None => match text.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        },
    };
    let port = match port {
        None => 443,
        Some(digits) => {
            let plain = !digits.is_empty()
                && digits.len() <= 5
                && digits.bytes().all(|b| b.is_ascii_digit())
                && !(digits.len() > 1 && digits.starts_with('0'));
            match digits.parse::<u16>() {
                Ok(port) if plain && port != 0 => port,
                _ => return false,
            }
        }
    };
    port == origin.port() && host.eq_ignore_ascii_case(origin.host())
}

/// Whether the request has exactly one `Origin` header and it is the public origin, byte for byte.
pub fn origin_exact(origin: &PublicOrigin, headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::ORIGIN).iter();
    matches!((values.next(), values.next()), (Some(one), None) if one.as_bytes() == origin.text().as_bytes())
}

/// Whether a state-changing request comes from the page: its `Origin` is the public origin exactly, or it has no `Origin`
/// and exactly one `Sec-Fetch-Site` that says `same-origin`. When an `Origin` is present it decides alone, whatever
/// `Sec-Fetch-Site` says. `Origin: null`, another scheme, host or port, a trailing path, an empty value and a repeated header
/// are refused.
pub fn change_origin_ok(origin: &PublicOrigin, headers: &HeaderMap) -> bool {
    if headers.contains_key(header::ORIGIN) {
        return origin_exact(origin, headers);
    }
    let mut values = headers.get_all("sec-fetch-site").iter();
    matches!((values.next(), values.next()), (Some(one), None) if one.as_bytes() == b"same-origin")
}

/// Whether the request has exactly one `Content-Type` header and its media type is the one `kind` takes. Parameters such as
/// `charset=utf-8` are allowed; `application/jsonx` and `application/json-patch+json` are not `application/json`.
pub fn content_type_ok(headers: &HeaderMap, kind: BodyKind) -> bool {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    let (Some(one), None) = (values.next(), values.next()) else { return false };
    let Ok(text) = one.to_str() else { return false };
    let media = text.split(';').next().unwrap_or("").trim_matches([' ', '\t']);
    media.eq_ignore_ascii_case(kind.media_type())
}

/// Whether the request has exactly one anti-forgery header and it is `expected`.
pub fn token_header_ok(headers: &HeaderMap, expected: &str) -> bool {
    let mut values = headers.get_all(TOKEN_HEADER).iter();
    matches!((values.next(), values.next()), (Some(one), None) if token_ok(one.as_bytes(), expected))
}

/// A `Content-Length` that is repeated or is not a plain number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadLength;

/// The declared body length of a request: `Ok(None)` for none, `Err(BadLength)` for a repeated or malformed header.
pub fn declared_length(headers: &HeaderMap) -> Result<Option<u64>, BadLength> {
    let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
    let Some(one) = values.next() else { return Ok(None) };
    if values.next().is_some() {
        return Err(BadLength);
    }
    let text = one.to_str().map_err(|_| BadLength)?;
    if text.is_empty() || text.len() > 19 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BadLength);
    }
    text.parse::<u64>().map(Some).map_err(|_| BadLength)
}

/// The info string of the token derivation.
const TOKEN_INFO: &[u8] = b"holzwave-server/v1/csrf-token";

/// The anti-forgery token of the session whose stored id is `session_id`: HKDF-SHA256 keyed by the `csrf` purpose key, with
/// the stored id (the hash of the cookie value) as the salt, as 43 base64url characters. Nothing is stored. The token changes
/// with every session, and a person who holds only the cookie value cannot make it, because the key is not theirs.
pub fn token_for(key: &[u8; KEY_BYTES], session_id: &TokenHash) -> String {
    let hkdf = Hkdf::<Sha256>::new(Some(&session_id[..]), &key[..]);
    let mut out = [0u8; 32];
    hkdf.expand(TOKEN_INFO, &mut out).expect("32 bytes is a valid HKDF-SHA256 output length");
    URL_SAFE_NO_PAD.encode(out)
}

/// Whether `given` is `expected`, compared in constant time. A different length is a mismatch at once: the length of the
/// token is public.
pub fn token_ok(given: &[u8], expected: &str) -> bool {
    given.ct_eq(expected.as_bytes()).into()
}
