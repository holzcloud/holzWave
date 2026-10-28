//! The built page, embedded in the binary and served with content types and cache headers.
//!
//! The files of the page's build output (`app/dist`) are part of the program. A release build holds them in memory; a debug
//! build reads the same folder from disk when the table is made, so the tests see the real output. The table of files is made
//! once, and a request is answered from it: a path is looked up exactly as it arrived and nothing is ever opened for it.
//!
//! What may be served without a session is the explicit list [`PUBLIC_FILES`] with the files under [`PUBLIC_PREFIX`]; every
//! other file needs a session.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use super::router::{Access, MaybeAuthed, Part};
use crate::log::{self, Level};

/// The folder the page is built into. A folder that does not exist embeds nothing, and [`verify`] refuses to start with an
/// empty page.
#[derive(RustEmbed)]
#[folder = "../app/dist"]
#[allow_missing = true]
struct Dist;

/// The files the sign-in page needs, which anyone may fetch: the page, its script and its styles, and the modules the script
/// imports. The test that follows the imports of the built script keeps this list equal to what the page really uses.
pub const PUBLIC_FILES: &[&str] = &[
    "/login.html",
    "/login.js",
    "/login.css",
    "/app.css",
    "/bridge.js",
    "/webhttp.js",
    "/webtransport.js",
    "/loginstate.js",
    "/loginpasskey.js",
    "/secretsteps.js",
    "/favicon.ico",
];

/// Everything under this prefix is public: the fonts the styles name.
pub const PUBLIC_PREFIX: &str = "/fonts/";

/// The closed list of file extensions the server serves and the content type of each. A file with another extension (a source
/// map, for example) is neither served nor listed.
pub const EXTENSIONS: &[(&str, &str)] = &[
    ("html", "text/html; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("woff2", "font/woff2"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("ico", "image/x-icon"),
    ("json", "application/json"),
    ("txt", "text/plain; charset=utf-8"),
];

/// Whether a file may be served without a session.
pub fn is_public(path: &str) -> bool {
    PUBLIC_FILES.contains(&path) || (path.starts_with(PUBLIC_PREFIX) && path.len() > PUBLIC_PREFIX.len())
}

/// The content type of a file by its extension, or `None` when the extension is not on the closed list. The extension is
/// matched exactly, in lower case.
pub fn content_type(path: &str) -> Option<&'static str> {
    let (_, extension) = path.rsplit_once('.')?;
    EXTENSIONS.iter().find(|(known, _)| *known == extension).map(|(_, content_type)| *content_type)
}

/// Whether a name (relative to the build folder, no leading slash) can be served: characters from a short set, no empty
/// segment and no dot segment. Anything else is left out of the table, so a path with such a character never matches.
fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && name.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// One file of the page.
#[derive(Debug)]
pub struct Asset {
    /// The path as a request names it, with the leading slash.
    pub path: String,
    /// The `Content-Type`.
    pub content_type: &'static str,
    /// The strong entity tag: the SHA-256 of the content in hex, in quotes.
    pub etag: String,
    /// The content.
    pub body: Bytes,
}

impl Asset {
    fn new(path: String, content_type: &'static str, body: Bytes) -> Asset {
        let etag = format!("\"{}\"", Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect::<String>());
        Asset { path, content_type, etag, body }
    }

    fn html(&self) -> bool {
        self.content_type.starts_with("text/html")
    }
}

/// The table of files, made once.
#[derive(Debug, Default)]
pub struct Assets {
    files: BTreeMap<String, Arc<Asset>>,
    skipped: usize,
}

impl Assets {
    /// A table of the given files (`name` without a leading slash). A file whose name is not allowed or whose extension is not
    /// on the closed list is left out and counted.
    pub fn from_files(files: impl IntoIterator<Item = (String, Bytes)>) -> Assets {
        let mut table = BTreeMap::new();
        let mut skipped = 0;
        for (name, body) in files {
            match content_type(&name).filter(|_| name_ok(&name)) {
                Some(content_type) => {
                    let path = format!("/{name}");
                    table.insert(path.clone(), Arc::new(Asset::new(path, content_type, body)));
                }
                None => skipped += 1,
            }
        }
        Assets { files: table, skipped }
    }

    /// The table of the embedded page, made on first use.
    pub fn embedded() -> &'static Assets {
        static TABLE: OnceLock<Assets> = OnceLock::new();
        TABLE.get_or_init(|| {
            let table = Assets::from_files(Dist::iter().filter_map(|name| {
                let file = Dist::get(&name)?;
                let body = match file.data {
                    std::borrow::Cow::Borrowed(bytes) => Bytes::from_static(bytes),
                    std::borrow::Cow::Owned(bytes) => Bytes::from(bytes),
                };
                Some((name.into_owned(), body))
            }));
            log::event(
                Level::Info,
                "page_loaded",
                &[("files", &table.len().to_string()), ("left_out", &table.skipped().to_string())],
            );
            table
        })
    }

    /// The file at `path` (with the leading slash), looked up exactly.
    pub fn get(&self, path: &str) -> Option<&Arc<Asset>> {
        self.files.get(path)
    }

    /// The paths in the table, in order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    /// How many files were left out of the table because their name or extension is not allowed.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// How many files are in the table.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The page is not part of this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageNotBuilt;

impl fmt::Display for PageNotBuilt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The page is not built. Run npm --prefix app run build before building the server.")
    }
}

impl std::error::Error for PageNotBuilt {}

/// Refuses a list of file names (relative to the build folder) that lacks `index.html` or `login.html`.
pub fn check_names<S: AsRef<str>>(names: &[S]) -> Result<(), PageNotBuilt> {
    let has = |wanted: &str| names.iter().any(|name| name.as_ref() == wanted);
    if has("index.html") && has("login.html") { Ok(()) } else { Err(PageNotBuilt) }
}

/// Refuses a build whose page is missing: the files the server would serve must include `index.html` and `login.html`.
pub fn verify() -> Result<(), PageNotBuilt> {
    let names: Vec<&str> = Assets::embedded().paths().map(|path| &path[1..]).collect();
    check_names(&names)
}

/// Whether an `If-None-Match` header names the tag (weak comparison, as the rule for this header says), or is a star.
fn tag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|text| {
            text.split(',').map(str::trim).any(|candidate| candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag)
        })
    })
}

/// The answer for a file: the whole content, or a 304 without a body when the request already holds this version.
fn respond(asset: &Asset, headers: &HeaderMap) -> Response {
    let html = asset.html();
    if !html && tag_matches(headers, &asset.etag) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        fill_headers(&mut response, asset);
        return response;
    }
    let mut response = Response::new(Body::from(asset.body.clone()));
    fill_headers(&mut response, asset);
    response.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(asset.body.len()));
    response
}

/// The headers an answer and a 304 share: the type, the cache rule and, except for HTML, the tag.
fn fill_headers(response: &mut Response, asset: &Asset) {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    if asset.html() {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        if let Ok(value) = HeaderValue::from_str(&asset.etag) {
            headers.insert(header::ETAG, value);
        }
    }
}

/// The parts of the application that serve the page: the files of the sign-in page and `GET /` (the sign-in page, or the
/// application page for someone who has a session) without a session, every other file with one.
pub fn parts() -> Vec<Part> {
    parts_of(Assets::embedded())
}

/// As [`parts`], for a given table. A part without a route is left out.
pub fn parts_of(assets: &'static Assets) -> Vec<Part> {
    let mut public = Router::new();
    let mut session = Router::new();
    let (mut public_routes, mut session_routes) = (0, 0);
    for path in assets.paths() {
        let Some(asset) = assets.get(path).cloned() else { continue };
        let route = get(move |headers: HeaderMap| {
            let asset = asset.clone();
            async move { respond(&asset, &headers) }
        });
        if is_public(path) {
            public = public.route(path, route);
            public_routes += 1;
        } else {
            session = session.route(path, route);
            session_routes += 1;
        }
    }
    if let (Some(login), Some(index)) = (assets.get("/login.html").cloned(), assets.get("/index.html").cloned()) {
        public = public.route(
            "/",
            get(move |MaybeAuthed(who): MaybeAuthed, headers: HeaderMap| {
                let asset = if who.is_some() { index.clone() } else { login.clone() };
                async move { respond(&asset, &headers) }
            }),
        );
        public_routes += 1;
    }
    let mut parts = Vec::new();
    if public_routes > 0 {
        parts.push(Part { access: Access::Public, router: public });
    }
    if session_routes > 0 {
        parts.push(Part { access: Access::Session, router: session });
    }
    parts
}
