//! The routes that move files between the browser and the person's home directory.
//!
//! `PUT /api/upload?name=<file name>` takes a file as the raw request body and has the person's own worker write it into the
//! upload folder of the person's home. The route checks everything it can before it reads a byte (the name, the declared
//! length, the number of uploads running), cleans the name, and then passes the body on in pieces of at most 1 MiB, one in
//! flight, to the worker. The worker never trusts what it gets, and the supervisor opens no path. An upload that ends in any
//! way but a complete body is aborted, and the worker removes what it wrote.
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

use std::collections::HashMap;
use std::future::Future;
use std::path::{Component, Path};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Query, Request};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Extension, Router};
use holzwave_host::ranges::{CHUNK, Range, WHOLE, parse_range};
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use super::app::App;
use super::csrf;
use super::router::{Access, ApiError, Authed, Part};
use crate::auth::session::SessionManager;
use crate::limits;
use crate::store::AccountId;
use crate::supervisor::serve::Services;
use crate::supervisor::{AccountKey, FileError, Supervisor};
use crate::wire::{Ctl, FileWhat, ResBody, valid_token};
use crate::worker::files::valid_dir;

/// What the file routes need from the rest of the server.
struct FileState {
    supervisor: Supervisor,
    app: Arc<App>,
    sessions: Arc<SessionManager>,
    /// The upload folder below a person's home, as `a/b` (None when the configuration holds something the worker would
    /// refuse, which the configuration check prevents).
    folder: Option<String>,
    /// The largest upload in bytes.
    upload_max: u64,
    /// The uploads running.
    uploads: Arc<Slots>,
    /// The downloads that are being streamed.
    downloads: Arc<Slots>,
}

/// The routes of this module: the download under a session and the upload.
pub fn parts(services: &Services) -> Vec<Part> {
    let limits = services.app.limits;
    let state = Arc::new(FileState {
        supervisor: services.supervisor.clone(),
        app: services.app.clone(),
        sessions: services.sessions.clone(),
        folder: folder_of(&services.upload.dir),
        upload_max: services.upload.max_bytes,
        uploads: Slots::new(limits.uploads_per_person, limits.uploads_total),
        downloads: Slots::new(limits.downloads_per_person, limits.downloads_total),
    });
    let download = Router::new().route("/files/{token}", get(download)).layer(Extension(state.clone()));
    let upload = Router::new().route("/api/upload", put(upload)).layer(Extension(state));
    vec![Part { access: Access::Session, router: download }, Part { access: Access::Upload, router: upload }]
}

/// The configured upload folder as the worker wants it: the plain components joined with `/`.
fn folder_of(dir: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in dir.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    let folder = parts.join("/");
    valid_dir(&folder).then_some(folder)
}

// ---- the places ---------------------------------------------------------------------------------------------------------

/// Counts the transfers of each person and of the whole server. A person's transfer holds a [`Permit`] for as long as it
/// runs; the place goes back when the permit drops, wherever the transfer ended.
struct Slots {
    per_person: usize,
    total: usize,
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    all: usize,
    by_person: HashMap<AccountId, usize>,
}

impl Slots {
    fn new(per_person: u32, total: u32) -> Arc<Slots> {
        let size = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
        Arc::new(Slots { per_person: size(per_person), total: size(total), held: Mutex::new(Held::default()) })
    }

    /// A place for `account`, or none when the person or the server is full.
    fn take(self: &Arc<Slots>, account: AccountId) -> Option<Permit> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let mine = held.by_person.get(&account).copied().unwrap_or(0);
        if held.all >= self.total || mine >= self.per_person {
            return None;
        }
        held.all += 1;
        held.by_person.insert(account, mine + 1);
        drop(held);
        Some(Permit { slots: self.clone(), account })
    }
}

/// A place in [`Slots`].
struct Permit {
    slots: Arc<Slots>,
    account: AccountId,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut held = self.slots.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.all = held.all.saturating_sub(1);
        if let Some(mine) = held.by_person.get_mut(&self.account) {
            *mine = mine.saturating_sub(1);
            if *mine == 0 {
                held.by_person.remove(&self.account);
            }
        }
    }
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
    /// The first piece, read before the head of the response was sent, so that a file that cannot be read at all is an error
    /// status and not a success status with a body that breaks.
    first: Option<Bytes>,
    /// The place of this download among the person's and the server's; it goes back when the body is dropped, whether it was
    /// read to the end, the client went away or the connection closed.
    _place: Permit,
}

impl HttpBody for FileBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        let this = &mut *self;
        if let Some(bytes) = this.first.take() {
            let length = bytes.len() as u64;
            this.offset += length;
            this.left = this.left.saturating_sub(length);
            return Poll::Ready(Some(Ok(Frame::data(bytes))));
        }
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
        self.first.is_none() && self.pending.is_none() && self.left == 0
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
fn plan(range_header: Option<&str>, size: u64) -> Result<Plan, Box<Response>> {
    match parse_range(range_header, size) {
        Range::Unsatisfiable => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            let headers = response.headers_mut();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{size}")) {
                headers.insert(header::CONTENT_RANGE, value);
            }
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
            Err(Box::new(hardened(response)))
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
        Range::Whole => Err(Box::new(too_large())),
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
        Err(response) => return *response,
    };
    let Ok(mime) = HeaderValue::from_str(&mime) else { return refuse(ApiError::Busy) };

    let head = request.method() == Method::HEAD;
    let body = if head || plan.length == 0 {
        Body::empty()
    } else {
        // A place for the stream: the next download of a person who has all theirs running is answered busy, not queued.
        let Some(place) = state.downloads.take(authed.account.id) else { return refuse(ApiError::Busy) };
        let first_length = u32::try_from(plan.length.min(limits::FILE_PIECE_BYTES as u64)).unwrap_or(0);
        let Ok(first) = read_piece(state.supervisor.clone(), key, token.clone(), plan.start, first_length).await else {
            // The grant was there a moment ago and the file cannot be read now: it is gone, as far as the client can tell.
            return refuse(ApiError::NotFound);
        };
        Body::new(FileBody {
            supervisor: state.supervisor.clone(),
            key,
            token,
            offset: plan.start,
            left: plan.length,
            pending: None,
            first: Some(first),
            _place: place,
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

// ---- the name -----------------------------------------------------------------------------------------------------------

/// The name given to a file whose own name is nothing a file system should hold.
pub const FALLBACK_NAME: &str = "upload";

/// Whether `c` is a character a name must not carry: a control character, an invisible formatting or direction character, or a
/// line or paragraph separator.
fn unwanted(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// The longest prefix of `text` that is at most `max` bytes and ends on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A file name that is safe to create, made from the name the page gave: the part after the last `/` or `\`, every control,
/// invisible or direction character as `_`, no space or dot at either end (so no hidden file and no `..`), at most
/// [`limits::UPLOAD_NAME_BYTES`] bytes on a character boundary with a short extension kept, and [`FALLBACK_NAME`] when nothing
/// is left. The worker checks the result again.
pub fn clean_name(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let replaced: String = last.chars().map(|c| if unwanted(c) { '_' } else { c }).collect();
    let trim = |text: &str| text.trim_matches(|c: char| c == ' ' || c == '.').to_owned();
    let mut name = trim(&replaced);
    if name.len() > limits::UPLOAD_NAME_BYTES {
        let extension = name
            .rfind('.')
            .filter(|at| *at > 0 && name.len() - *at <= limits::UPLOAD_EXTENSION_KEEP)
            .map(|at| name[at..].to_owned());
        name = match extension {
            Some(extension) => {
                let stem = trim(cut(&name[..name.len() - extension.len()], limits::UPLOAD_NAME_BYTES - extension.len()));
                if stem.is_empty() { cut(&format!("{FALLBACK_NAME}{extension}"), limits::UPLOAD_NAME_BYTES).to_owned() } else { format!("{stem}{extension}") }
            }
            None => trim(cut(&name, limits::UPLOAD_NAME_BYTES)),
        };
    }
    if name.is_empty() { FALLBACK_NAME.to_owned() } else { name }
}

/// The name of the upload from the query: exactly one parameter, `name`, of reasonable length.
fn name_of(uri: &Uri) -> Result<String, ApiError> {
    let bad = ApiError::BadRequest { field: Some("name") };
    let raw = uri.query().ok_or(bad)?;
    if raw.len() > limits::UPLOAD_QUERY_BYTES {
        return Err(bad);
    }
    let Query(pairs) = Query::<Vec<(String, String)>>::try_from_uri(uri).map_err(|_| bad)?;
    let [(key, value)] = pairs.as_slice() else { return Err(bad) };
    if key != "name" || value.len() > limits::UPLOAD_NAME_INPUT_BYTES {
        return Err(bad);
    }
    Ok(clean_name(value))
}

/// The length the request declares, which must be there, must not be chunked and must not be over the limit.
fn declared_size(headers: &HeaderMap, max: u64) -> Result<u64, ApiError> {
    let bad = ApiError::BadRequest { field: Some("size") };
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return Err(bad);
    }
    match csrf::declared_length(headers) {
        Ok(Some(size)) if size <= max => Ok(size),
        _ => Err(bad),
    }
}

// ---- the upload ---------------------------------------------------------------------------------------------------------

/// What the worker answers to the close of an upload that was kept.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    path: String,
    size: u64,
}

/// The path and size of a kept upload, if the worker's answer is what a worker may say: UTF-8, absolute, bounded, without a
/// control character and with the size that was declared.
fn read_stored(text: &str, size: u64) -> Option<Stored> {
    if text.len() > limits::UPLOAD_PATH_BYTES + 64 {
        return None;
    }
    let stored: Stored = serde_json::from_str(text).ok()?;
    let fine = stored.size == size
        && stored.path.starts_with('/')
        && stored.path.len() <= limits::UPLOAD_PATH_BYTES
        && !stored.path.chars().any(char::is_control);
    fine.then_some(stored)
}

/// An upload that is open in a worker. Ending it is [`Transfer::abort`] or a close; when the handler is dropped in between (the
/// client went away) the drop sends the abort, so the worker removes the part file whatever happened.
struct Transfer {
    supervisor: Supervisor,
    key: AccountKey,
    id: u64,
    open: bool,
    permit: Option<Permit>,
}

impl Transfer {
    /// Tells the worker to drop the upload and remove what it wrote.
    async fn abort(&mut self) {
        if self.open {
            self.open = false;
            let _ = self.supervisor.file_call(self.key, Ctl::UploadClose { id: self.id, commit: false }, false).await;
        }
    }

    /// One piece, answered when it is on the disk. An answer of the worker that is not a success has ended the upload there.
    async fn piece(&mut self, bytes: Vec<u8>) -> Result<(), ApiError> {
        match self.supervisor.file_call(self.key, Ctl::UploadPiece { id: self.id, bytes }, false).await {
            Ok(ResBody::Ok(_)) => Ok(()),
            Ok(_) => {
                self.open = false;
                Err(ApiError::Busy)
            }
            Err(_) => Err(ApiError::Busy),
        }
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let (supervisor, key, id, permit) = (self.supervisor.clone(), self.key, self.id, self.permit.take());
        runtime.spawn(async move {
            let _ = supervisor.file_call(key, Ctl::UploadClose { id, commit: false }, false).await;
            drop(permit);
        });
    }
}

/// Whether the session has ended; a store that cannot be read counts as ended.
async fn session_gone(state: &FileState, authed: &Authed) -> bool {
    let (sessions, session, now) = (state.sessions.clone(), authed.session.clone(), state.app.clock.unix());
    tokio::task::spawn_blocking(move || sessions.check(&session, now).is_err()).await.unwrap_or(true)
}

async fn upload(request: Request) -> Response {
    upload_checked(request).await.unwrap_or_else(IntoResponse::into_response)
}

async fn upload_checked(request: Request) -> Result<Response, ApiError> {
    let state = request.extensions().get::<Arc<FileState>>().cloned().ok_or(ApiError::Busy)?;
    let authed = request.extensions().get::<Authed>().cloned().ok_or(ApiError::Invalid)?;
    let key = AccountKey { id: authed.account.id, uid: authed.account.uid };
    if request.method() != Method::PUT {
        return Err(ApiError::MethodNotAllowed);
    }
    // Everything that can be refused before a byte of the body is read.
    let name = name_of(request.uri())?;
    let size = declared_size(request.headers(), state.upload_max)?;
    let folder = state.folder.clone().ok_or(ApiError::Busy)?;
    let permit = state.uploads.take(authed.account.id).ok_or(ApiError::Busy)?;

    let id = state.supervisor.file_id();
    let mut transfer = Transfer { supervisor: state.supervisor.clone(), key, id, open: true, permit: Some(permit) };
    let open = Ctl::UploadOpen { id, size, folder, name };
    match state.supervisor.file_call(key, open, true).await {
        Ok(ResBody::Ok(_)) => {}
        Ok(_) => {
            transfer.open = false;
            return Err(ApiError::Busy);
        }
        Err(_) => {
            transfer.abort().await;
            return Err(ApiError::Busy);
        }
    }
    let outcome = receive(&state, &authed, &mut transfer, request, size).await;
    if outcome.is_err() {
        transfer.abort().await;
    }
    outcome
}

/// Reads the body, passes it on and closes the upload.
async fn receive(
    state: &FileState,
    authed: &Authed,
    transfer: &mut Transfer,
    request: Request,
    size: u64,
) -> Result<Response, ApiError> {
    let stall = state.app.limits.upload_stall;
    let mut ended = state.sessions.subscribe_ended();
    // A sign-out between the check of the guard and the subscription is not announced to this receiver.
    if session_gone(state, authed).await {
        return Err(ApiError::Invalid);
    }
    let mut body = request.into_body();
    let mut buffer: Vec<u8> = Vec::new();
    let mut received: u64 = 0;
    loop {
        let frame = tokio::select! {
            told = ended.recv() => {
                match told {
                    Ok(ended) if ended.ends(&authed.session) => return Err(ApiError::Invalid),
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        if session_gone(state, authed).await {
                            return Err(ApiError::Invalid);
                        }
                    }
                    Err(RecvError::Closed) => return Err(ApiError::Busy),
                }
                continue;
            }
            frame = tokio::time::timeout(stall, body.frame()) => frame,
        };
        let Ok(frame) = frame else { return Err(ApiError::BadRequest { field: None }) };
        match frame {
            None => break,
            Some(Err(_)) => return Err(ApiError::BadRequest { field: None }),
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else { continue };
                received += data.len() as u64;
                if received > size {
                    return Err(ApiError::BadRequest { field: Some("size") });
                }
                buffer.extend_from_slice(&data);
                while buffer.len() >= limits::FILE_PIECE_BYTES {
                    let rest = buffer.split_off(limits::FILE_PIECE_BYTES);
                    let piece = std::mem::replace(&mut buffer, rest);
                    transfer.piece(piece).await?;
                }
            }
        }
    }
    if received != size {
        return Err(ApiError::BadRequest { field: Some("size") });
    }
    if !buffer.is_empty() {
        transfer.piece(std::mem::take(&mut buffer)).await?;
    }
    match transfer.supervisor.file_call(transfer.key, Ctl::UploadClose { id: transfer.id, commit: true }, false).await {
        Ok(ResBody::Ok(text)) => {
            transfer.open = false;
            let stored = read_stored(&text, size).ok_or(ApiError::Busy)?;
            let mut response = Json(serde_json::json!({ "path": stored.path, "size": stored.size })).into_response();
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            Ok(response)
        }
        Ok(_) => {
            transfer.open = false;
            Err(ApiError::Busy)
        }
        Err(_) => Err(ApiError::Busy),
    }
}
