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
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use super::router::{Access, MaybeAuthed, Part};

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

/// Whether a file may be served without a session.
pub fn is_public(path: &str) -> bool {
    PUBLIC_FILES.contains(&path) || (path.starts_with(PUBLIC_PREFIX) && path.len() > PUBLIC_PREFIX.len())
}

/// The content type of a file by its extension.
pub fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
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
    fn new(path: String, body: Bytes) -> Asset {
        let etag = format!("\"{}\"", Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect::<String>());
        let content_type = content_type(&path);
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
}

impl Assets {
    /// A table of the given files (`name` without a leading slash).
    pub fn from_files(files: impl IntoIterator<Item = (String, Bytes)>) -> Assets {
        let files = files
            .into_iter()
            .map(|(name, body)| {
                let path = format!("/{name}");
                (path.clone(), Arc::new(Asset::new(path, body)))
            })
            .collect();
        Assets { files }
    }

    /// The table of the embedded page, made on first use.
    pub fn embedded() -> &'static Assets {
        static TABLE: OnceLock<Assets> = OnceLock::new();
        TABLE.get_or_init(|| {
            Assets::from_files(Dist::iter().filter_map(|name| {
                let file = Dist::get(&name)?;
                let body = match file.data {
                    std::borrow::Cow::Borrowed(bytes) => Bytes::from_static(bytes),
                    std::borrow::Cow::Owned(bytes) => Bytes::from(bytes),
                };
                Some((name.into_owned(), body))
            }))
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

/// Refuses a build whose page is missing.
pub fn verify() -> Result<(), PageNotBuilt> {
    Ok(())
}

fn respond(asset: &Asset) -> Response {
    let mut response = Response::new(Body::from(asset.body.clone()));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(asset.body.len()));
    if asset.html() {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    } else {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        if let Ok(value) = HeaderValue::from_str(&asset.etag) {
            headers.insert(header::ETAG, value);
        }
    }
    *response.status_mut() = StatusCode::OK;
    response
}

/// The parts of the application that serve the page: the files of the sign-in page and `GET /` (the sign-in page, or the
/// application page for someone who has a session) without a session, every other file with one.
pub fn parts() -> Vec<Part> {
    parts_of(Assets::embedded())
}

/// As [`parts`], for a given table.
pub fn parts_of(assets: &'static Assets) -> Vec<Part> {
    let mut public = Router::new();
    let mut session = Router::new();
    for path in assets.paths() {
        let Some(asset) = assets.get(path).cloned() else { continue };
        let route = get(move || {
            let asset = asset.clone();
            async move { respond(&asset) }
        });
        if is_public(path) {
            public = public.route(path, route);
        } else {
            session = session.route(path, route);
        }
    }
    if let (Some(login), Some(index)) = (assets.get("/login.html").cloned(), assets.get("/index.html").cloned()) {
        public = public.route(
            "/",
            get(move |MaybeAuthed(who): MaybeAuthed| {
                let asset = if who.is_some() { index.clone() } else { login.clone() };
                async move { respond(&asset) }
            }),
        );
    }
    vec![Part { access: Access::Public, router: public }, Part { access: Access::Session, router: session }]
}
