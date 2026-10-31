//! The routes that move files between the browser and the person's home directory.
//!
//! `GET /files/{token}` (and `HEAD`) serves a file that the person's own `file_grant` made available, with the answers of the
//! desktop's `holzfile` scheme: the whole file when it is small and no range was asked, `206` for one range of at most the
//! chunk limit, `416` for a range that cannot be satisfied and `413` for a large file asked for without a range. The route
//! never opens a path: the supervisor asks the person's worker through the file frames, and the worker answers the grant's
//! media type and size and then the bytes, read by the core's own `granted` and `read_granted`. A token belongs to one person's
//! worker, so a token of one person used with another person's session is unknown, exactly like a token that was never made.
//!
//! The body is streamed. It is a [`FileBody`] that asks the worker for the next piece (at most 1 MiB) only when the connection
//! takes the one before, so a response holds one piece, a client that goes away stops the reads, and a worker that fails ends
//! the response with a closed connection, never with a short body and a success status.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::header::{self, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use holzwave_host::ranges::{CHUNK, Range, WHOLE, parse_range};
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use serde::Deserialize;

use super::router::{Access, ApiError, Authed, Part};
use crate::limits;
use crate::supervisor::serve::Services;
use crate::supervisor::{AccountKey, FileError, Supervisor};
use crate::wire::{Ctl, FileWhat, ResBody, valid_token};

/// What a download of a file needs from the rest of the server.
struct FileState {
    supervisor: Supervisor,
}

/// The routes of this module: the download under a session.
pub fn parts(services: &Services) -> Vec<Part> {
    let state = Arc::new(FileState { supervisor: services.supervisor.clone() });
    let download = Router::new().route("/files/{token}", get(download)).layer(Extension(state));
    vec![Part { access: Access::Session, router: download }]
}

// ---- the headers --------------------------------------------------------------------------------------------------------

const NOSNIFF: HeaderName = HeaderName::from_static("x-content-type-options");
const SANDBOX: HeaderName = HeaderName::from_static("content-security-policy");
const SAME_SITE: HeaderName = HeaderName::from_static("cross-origin-resource-policy");

/// The policy of every response of this route: nothing loads and nothing runs, whatever the file is.
pub const FILE_POLICY: &str = "default-src 'none'; sandbox";

/// Puts the headers on a response of this route that decide what a file may do in the page's origin. The application layer
/// keeps them as they are.
fn hardened(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(NOSNIFF, HeaderValue::from_static("nosniff"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(SANDBOX, HeaderValue::from_static(FILE_POLICY));
    headers.insert(SAME_SITE, HeaderValue::from_static("same-site"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response
}

/// An error answer with the headers of this route.
fn refuse(error: ApiError) -> Response {
    hardened(error.into_response())
}

/// 413 with the field `range`: a file larger than the whole-file limit must be asked for in ranges.
fn too_large() -> Response {
    let body = r#"{"code":"bad_request","field":"range"}"#;
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::PAYLOAD_TOO_LARGE;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    hardened(response)
}

// ---- what the worker says -----------------------------------------------------------------------------------------------

/// The answer to a stat.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stat {
    mime: String,
    size: u64,
}

/// Whether `mime` can be the media type of a file: `type/subtype` in lower case, 3 to 127 bytes, no parameter.
fn media_type_ok(mime: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-')
            })
            && part.as_bytes()[0].is_ascii_alphanumeric()
    };
    mime.len() <= limits::MEDIA_TYPE_BYTES && mime.split_once('/').is_some_and(|(kind, sub)| plain(kind) && plain(sub))
}

/// The media type and size a worker answered to a stat, if the answer is what a worker may say.
fn read_stat(text: &str) -> Option<(String, u64)> {
    if text.len() > 512 {
        return None;
    }
    let stat: Stat = serde_json::from_str(text).ok()?;
    media_type_ok(&stat.mime).then_some((stat.mime, stat.size))
}

/// The token in a request path: exactly `/files/` and 32 lower-case hex digits, as the client wrote them.
fn token_of(path: &str) -> Option<&str> {
    let token = path.strip_prefix("/files/")?;
    valid_token(token).then_some(token)
}

// ---- the body -----------------------------------------------------------------------------------------------------------

type Piece = Pin<Box<dyn Future<Output = Result<Bytes, ()>> + Send>>;

/// Asks the worker for `length` bytes of the grant from `offset`. An answer of another length is a failure: the response was
/// promised a length.
async fn read_piece(supervisor: Supervisor, key: AccountKey, token: String, offset: u64, length: u32) -> Result<Bytes, ()> {
    let request = supervisor.file_id();
    let control = Ctl::FileServe { request, what: FileWhat::Read, token, offset, length };
    match supervisor.file_call(key, control, false).await {
        Ok(ResBody::Binary(bytes)) if bytes.len() == usize::try_from(length).unwrap_or(usize::MAX) => Ok(Bytes::from(bytes)),
        _ => Err(()),
    }
}

/// The body of a download: the next piece is asked for when the connection asks for the next frame, so at most one piece is
/// held. A failed read ends the body with an error, which closes the connection before the promised length was sent.
struct FileBody {
    supervisor: Supervisor,
    key: AccountKey,
    token: String,
    offset: u64,
    left: u64,
    pending: Option<(Piece, u32)>,
}

impl HttpBody for FileBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        let this = &mut *self;
        if this.pending.is_none() {
            if this.left == 0 {
                return Poll::Ready(None);
            }
            let piece = u32::try_from(this.left.min(limits::FILE_PIECE_BYTES as u64)).unwrap_or(0);
            let future = read_piece(this.supervisor.clone(), this.key, this.token.clone(), this.offset, piece);
            this.pending = Some((Box::pin(future), piece));
        }
        let Some((future, piece)) = this.pending.as_mut() else { return Poll::Ready(None) };
        let piece = *piece;
        match future.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(bytes)) => {
                this.pending = None;
                this.offset += u64::from(piece);
                this.left -= u64::from(piece);
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(Err(())) => {
                this.pending = None;
                this.left = 0;
                Poll::Ready(Some(Err(std::io::Error::other("the file could not be read"))))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.pending.is_none() && self.left == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.left)
    }
}

// ---- the download -------------------------------------------------------------------------------------------------------

/// What the request asks for, decided from the size of the file.
struct Plan {
    status: StatusCode,
    start: u64,
    length: u64,
    range: Option<String>,
}

/// The answer to a range header for a file of `size` bytes, as the desktop's file scheme gives it: `Err` is the response to
/// send instead.
fn plan(range_header: Option<&str>, size: u64) -> Result<Plan, Response> {
    match parse_range(range_header, size) {
        Range::Unsatisfiable => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            let headers = response.headers_mut();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{size}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            Err(hardened(response))
        }
        Range::Bytes(start, end) => {
            let end = end.min(start.saturating_add(CHUNK - 1));
            Ok(Plan {
                status: StatusCode::PARTIAL_CONTENT,
                start,
                length: end - start + 1,
                range: Some(format!("bytes {start}-{end}/{size}")),
            })
        }
        Range::Whole if size <= WHOLE => Ok(Plan { status: StatusCode::OK, start: 0, length: size, range: None }),
        Range::Whole => Err(too_large()),
    }
}

/// The value of the only `Range` header of a request, if there is exactly one and it is text.
fn range_header(request: &Request) -> Option<&str> {
    let mut values = request.headers().get_all(header::RANGE).iter();
    let one = values.next()?;
    if values.next().is_some() {
        return None;
    }
    one.to_str().ok()
}

async fn download(request: Request) -> Response {
    let Some(state) = request.extensions().get::<Arc<FileState>>().cloned() else {
        return refuse(ApiError::Busy);
    };
    let Some(authed) = request.extensions().get::<Authed>() else { return refuse(ApiError::Invalid) };
    let key = AccountKey { id: authed.account.id, uid: authed.account.uid };
    let Some(token) = token_of(request.uri().path()) else { return refuse(ApiError::NotFound) };
    let token = token.to_owned();

    let id = state.supervisor.file_id();
    let stat = Ctl::FileServe { request: id, what: FileWhat::Stat, token: token.clone(), offset: 0, length: 0 };
    let (mime, size) = match state.supervisor.file_call(key, stat, false).await {
        Ok(ResBody::Ok(text)) => match read_stat(&text) {
            Some(found) => found,
            None => return refuse(ApiError::Busy),
        },
        // An unknown token, a token of another person (whose worker has no such grant), a person with no worker and a
        // grant that expired are one answer.
        Ok(_) | Err(FileError::NoWorker | FileError::WorkerGone | FileError::Refused) => return refuse(ApiError::NotFound),
        Err(_) => return refuse(ApiError::Busy),
    };
    let plan = match plan(range_header(&request), size) {
        Ok(plan) => plan,
        Err(response) => return response,
    };
    let Ok(mime) = HeaderValue::from_str(&mime) else { return refuse(ApiError::Busy) };

    let head = request.method() == Method::HEAD;
    let body = if head || plan.length == 0 {
        Body::empty()
    } else {
        Body::new(FileBody {
            supervisor: state.supervisor.clone(),
            key,
            token,
            offset: plan.start,
            left: plan.length,
            pending: None,
        })
    };
    let mut response = Response::new(body);
    *response.status_mut() = plan.status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(plan.length));
    if let Some(range) = plan.range
        && let Ok(value) = HeaderValue::from_str(&range)
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    hardened(response)
}
