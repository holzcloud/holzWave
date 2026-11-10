//! The routes of the server as a sync target, under `/sync/v1`: a small mirror of the sync engine's own backend, with the device
//! token as the only credential.
//!
//! - `POST /sync/v1/ensure/{dir}`: the folder `-` (the root), `files` or `backups` exists; there is nothing to make (a person's
//!   files are rows), so it answers 204 for a name of the three and 400 for any other.
//! - `GET /sync/v1/list/{dir}`: a JSON array of `{name, etag, checksum}` for the direct children of the folder. The tag and the
//!   checksum are the same value, the lowercase hex SHA-256 of the stored bytes.
//! - `GET /sync/v1/blob/{path}`: the stored bytes, with the tag in `ETag`, or 404.
//! - `PUT /sync/v1/blob/{path}`: stores the body. `If-None-Match: *` writes only a file that is not there, `If-Match: "<tag>"`
//!   only a file with that tag, and no condition always; a failed condition is 412. 201 for a new file, 200 for a replaced one.
//!
//! What matters here and not in the module of the rules ([`crate::sync_target`]):
//!
//! - **Behind the gate and the token.** Every path is under `/sync/v1/`, which the edge answers as a path that is not there
//!   while the sync switch is off, before any credential is read; a request that gets past it needs a bearer token of a device
//!   of an active account, and a cookie is never looked at. Every query is scoped by that device's account, so one person can
//!   not name another person's file, only an own file of the same name.
//! - **The path is read raw.** It is taken from the request's address and never decoded: a `%` is no character of a name, so an
//!   encoded slash is not a slash and the answer is 400, not a file.
//! - **The body is read by this route, and only as far as it may go.** The device guard buffers nothing. A declared length over
//!   the file's cap is refused before a byte is read, a body without a length is read only up to the cap, and the read has a time
//!   limit, so a slow sender holds a connection for a bounded time and never memory beyond one file.
//! - **Nothing is written down.** No path, no name, no tag and no byte of a body reaches a log or the audit log; a request that
//!   is refused is answered with a fixed word and nothing the caller sent.
//! - **Every store call runs off the async threads**, and the write is one immediate transaction.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::header::{self, HeaderMap, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use http_body_util::BodyExt;
use serde_json::json;

use super::app::App;
use super::router::{Access, ApiError, DeviceAuthed, Part};
use super::routes_auth::{Refusal, answer, blocking};
use super::routes_me::no_content;
use crate::limits::SYNC_BODY_TIMEOUT;
use crate::sync_target::{self, BlobPath, Bounds, Folder, Written};

const ENSURE: &str = "/sync/v1/ensure/";
const LIST: &str = "/sync/v1/list/";
const BLOB: &str = "/sync/v1/blob/";

fn bad() -> Refusal {
    Refusal(ApiError::BadRequest { field: None })
}

fn busy() -> Refusal {
    Refusal(ApiError::Busy)
}

/// What follows `prefix` in the raw path of the request, or `None`. Nothing is decoded.
fn tail<'a>(uri: &'a Uri, prefix: &str) -> Option<&'a str> {
    uri.path().strip_prefix(prefix)
}

/// The folder a request names.
fn folder_of(uri: &Uri, prefix: &str) -> Result<Folder, Refusal> {
    tail(uri, prefix).and_then(Folder::parse).ok_or_else(bad)
}

/// The blob a request names.
fn path_of(uri: &Uri) -> Result<BlobPath, Refusal> {
    tail(uri, BLOB).and_then(BlobPath::parse).ok_or_else(bad)
}

/// The value of a header that may be there at most once and must be plain text: `Ok(None)` when it is not there.
fn one_header(headers: &HeaderMap, name: header::HeaderName) -> Result<Option<&str>, Refusal> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else { return Ok(None) };
    if values.next().is_some() {
        return Err(bad());
    }
    first.to_str().map(Some).map_err(|_| bad())
}

/// A tag as a header value: quoted. Hex digits only, so the value is always valid.
fn tag_value(etag: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{etag}\"")).unwrap_or_else(|_| HeaderValue::from_static("\"\""))
}

/// `POST /sync/v1/ensure/{dir}`.
async fn ensure_route(uri: Uri, _who: DeviceAuthed) -> Result<Response, Refusal> {
    folder_of(&uri, ENSURE)?;
    Ok(no_content(Vec::new()))
}

/// `GET /sync/v1/list/{dir}`: the names, tags and checksums of the person's own files in the folder.
async fn list_route(Extension(app): Extension<Arc<App>>, uri: Uri, who: DeviceAuthed) -> Result<Response, Refusal> {
    let folder = folder_of(&uri, LIST)?;
    let account = who.account.id;
    let entries = blocking(move || app.store.read(|tx| tx.blob_list(account, folder.prefix())))
        .await?
        .map_err(|_| busy())?;
    let rows: Vec<_> = entries
        .iter()
        .map(|entry| json!({ "name": entry.name, "etag": entry.etag, "checksum": entry.etag }))
        .collect();
    Ok(answer(StatusCode::OK, &serde_json::Value::Array(rows), Vec::new()))
}

/// `GET /sync/v1/blob/{path}`.
async fn get_route(Extension(app): Extension<Arc<App>>, uri: Uri, who: DeviceAuthed) -> Result<Response, Refusal> {
    let path = path_of(&uri)?;
    let account = who.account.id;
    let key = path.key();
    let found = blocking(move || app.store.read(|tx| tx.blob_get(account, &key))).await?.map_err(|_| busy())?;
    let Some(blob) = found else { return Err(Refusal(ApiError::NotFound)) };
    let mut response = (StatusCode::OK, Bytes::from(blob.body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::ETAG, tag_value(&blob.etag));
    Ok(response)
}

/// Why a body could not be read.
enum ReadError {
    /// More bytes than the cap.
    TooLarge,
    /// The sender stopped, broke the connection or was too slow.
    Broken,
}

/// Reads a body of at most `cap` bytes. It stops at the first byte over the cap, so a longer body is never held.
async fn read_capped(mut body: Body, cap: usize) -> Result<Vec<u8>, ReadError> {
    let mut out: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ReadError::Broken)?;
        if let Ok(data) = frame.into_data() {
            if data.len() > cap.saturating_sub(out.len()) {
                return Err(ReadError::TooLarge);
            }
            out.extend_from_slice(&data);
        }
    }
    Ok(out)
}

fn refused(refusal: sync_target::Refusal) -> Refusal {
    Refusal(match refusal {
        sync_target::Refusal::BadRequest => ApiError::BadRequest { field: None },
        sync_target::Refusal::TooLarge => ApiError::TooLarge,
        sync_target::Refusal::PreconditionFailed => ApiError::PreconditionFailed,
        sync_target::Refusal::OverQuota => ApiError::Quota,
    })
}

/// `PUT /sync/v1/blob/{path}`.
async fn put_route(
    Extension(app): Extension<Arc<App>>,
    Extension(bounds): Extension<Bounds>,
    uri: Uri,
    headers: HeaderMap,
    who: DeviceAuthed,
    body: Body,
) -> Result<Response, Refusal> {
    let path = path_of(&uri)?;
    let cond = sync_target::condition(one_header(&headers, header::IF_NONE_MATCH)?, one_header(&headers, header::IF_MATCH)?)
        .ok_or_else(bad)?;
    // What can be refused without the body is refused before a byte of it is read.
    if path == BlobPath::Meta && cond != crate::store::Cond::Absent {
        return Err(bad());
    }
    let cap = path.body_cap(&bounds);
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > u64::try_from(cap).unwrap_or(u64::MAX)) {
        return Err(Refusal(ApiError::TooLarge));
    }
    let bytes = match tokio::time::timeout(Duration::from_secs(SYNC_BODY_TIMEOUT.as_secs()), read_capped(body, cap)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(ReadError::TooLarge)) => return Err(Refusal(ApiError::TooLarge)),
        Ok(Err(ReadError::Broken)) | Err(_) => return Err(bad()),
    };
    let (account, now) = (who.account.id, app.clock.unix());
    let written = blocking(move || app.store.write(|tx| sync_target::put(tx, account, &path, &bytes, &cond, &bounds, now)))
        .await?
        .map_err(|_| busy())?;
    let (status, etag) = match written {
        Written::Created(etag) => (StatusCode::CREATED, etag),
        Written::Replaced(etag) => (StatusCode::OK, etag),
        Written::Refused(why) => return Err(refused(why)),
    };
    let mut response = status.into_response();
    response.headers_mut().insert(header::ETAG, tag_value(&etag));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// The routes of this module: one part, a device with the sync switch on. `bounds` is what one person may keep.
pub fn parts(bounds: Bounds) -> Vec<Part> {
    let sync = Router::new()
        .route("/sync/v1/ensure/{dir}", post(ensure_route))
        .route("/sync/v1/list/{dir}", get(list_route))
        .route("/sync/v1/blob/{*path}", get(get_route).put(put_route))
        .layer(Extension(bounds));
    vec![Part { access: Access::Device, router: sync }]
}
