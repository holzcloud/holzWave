//! The `holzfile` URI scheme: how an image, a video, audio or a PDF is shown without loading the whole file into the
//! page and without a `blob:` URL (which the policy does not allow).
//!
//! A URL is `holzfile://localhost/<token>` (on Windows and Android `http://holzfile.localhost/<token>`). The token is
//! 128 random bits that `file_grant` made for one file; nothing else is served, there is no path and no directory, so a
//! page cannot ask for a file it was not given. Range requests are answered with `206`, in pieces of at most
//! [`CHUNK`] bytes, so a video starts and seeks at once. Every response says `nosniff`, `no-store` and a CSP of
//! `sandbox`, so even a file opened by itself could run nothing.

use tauri::http::{self, Method, Request, Response, StatusCode, header};

use crate::state::AppCore;

/// The scheme name, as registered and as in the policy.
pub const SCHEME: &str = "holzfile";
/// The most one ranged response carries.
pub const CHUNK: u64 = 8 * 1024 * 1024;
/// The most a response without a range carries; a larger file must be asked for in ranges.
pub const WHOLE: u64 = 64 * 1024 * 1024;

/// Where the page finds a granted file, which depends on the platform the way custom schemes do.
pub fn url_of(token: &str) -> String {
    if cfg!(any(windows, target_os = "android")) {
        format!("http://{SCHEME}.localhost/{token}")
    } else {
        format!("{SCHEME}://localhost/{token}")
    }
}

/// A byte range of a file of `size` bytes: `start..=end`, both inside the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Range {
    Whole,
    Bytes(u64, u64),
    /// The range cannot be satisfied (starts after the end).
    Unsatisfiable,
}

/// Read a `Range: bytes=...` header. One range only; anything else is the whole file, which a client must accept.
pub fn parse_range(header: Option<&str>, size: u64) -> Range {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return Range::Whole };
    if spec.contains(',') || size == 0 {
        return Range::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else { return Range::Whole };
    let (a, b) = (a.trim(), b.trim());
    let last = size - 1;
    let (start, end) = match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(start), Ok(end)) => (start, end.min(last)),
        (Ok(start), Err(_)) if b.is_empty() => (start, last),
        // `-N`: the last N bytes.
        (Err(_), Ok(n)) if a.is_empty() && n > 0 => (size.saturating_sub(n), last),
        _ => return Range::Whole,
    };
    if start > last || start > end { Range::Unsatisfiable } else { Range::Bytes(start, end) }
}

fn plain(status: StatusCode, text: &str) -> Response<Vec<u8>> {
    base(Response::builder().status(status).header(header::CONTENT_TYPE, "text/plain; charset=utf-8")).body(text.as_bytes().to_vec()).expect("a plain response")
}

fn base(builder: http::response::Builder) -> http::response::Builder {
    builder
        .header("X-Content-Type-Options", "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .header("Content-Security-Policy", "default-src 'none'; sandbox")
        .header("Cross-Origin-Resource-Policy", "same-site")
        .header(header::ACCEPT_RANGES, "bytes")
}

/// The token of a request URL: the whole path, and nothing else may be in it.
fn token_of(request: &Request<Vec<u8>>) -> Option<String> {
    let token = request.uri().path().strip_prefix('/')?;
    (token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())).then(|| token.to_owned())
}

/// Answer one request. Blocking (it reads a file, maybe over the network): call it off the main thread.
pub fn serve(core: &AppCore, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let head = match *request.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => return plain(StatusCode::METHOD_NOT_ALLOWED, "only GET and HEAD"),
    };
    let Some(token) = token_of(request) else { return plain(StatusCode::NOT_FOUND, "no such file") };
    let Some((mime, size)) = core.granted(&token) else { return plain(StatusCode::NOT_FOUND, "no such file") };
    let range = parse_range(request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()), size);
    let (status, start, end) = match range {
        Range::Unsatisfiable => {
            return base(Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{size}")))
                .body(Vec::new())
                .expect("a 416 response");
        }
        Range::Bytes(start, end) => (StatusCode::PARTIAL_CONTENT, start, end.min(start + CHUNK - 1)),
        Range::Whole if size <= WHOLE => (StatusCode::OK, 0, size.saturating_sub(1)),
        Range::Whole => return plain(StatusCode::PAYLOAD_TOO_LARGE, "ask for this file in ranges"),
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut builder = base(Response::builder().status(status))
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, length);
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    if head || length == 0 {
        return builder.body(Vec::new()).expect("a head response");
    }
    match core.read_granted(&token, start, length) {
        Some(Ok(bytes)) => builder.body(bytes).expect("a file response"),
        Some(Err(why)) => {
            eprintln!("holzwave: holzfile: {why}");
            plain(StatusCode::BAD_GATEWAY, "the file could not be read")
        }
        None => plain(StatusCode::NOT_FOUND, "no such file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_read_and_clamped_to_the_file() {
        assert_eq!(parse_range(None, 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-"), 100), Range::Bytes(0, 99));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Range::Bytes(10, 19));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Range::Bytes(90, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Range::Bytes(90, 99));
        assert_eq!(parse_range(Some("bytes=-500"), 100), Range::Bytes(0, 99));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=50-10"), 100), Range::Unsatisfiable);
    }

    #[test]
    fn what_is_not_one_plain_range_is_the_whole_file() {
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Range::Whole, "several ranges");
        assert_eq!(parse_range(Some("items=0-1"), 100), Range::Whole, "another unit");
        assert_eq!(parse_range(Some("bytes=x-y"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-5"), 0), Range::Whole, "an empty file has no ranges");
    }

    #[test]
    fn only_a_token_is_a_path() {
        let req = |uri: &str| Request::builder().uri(uri).body(Vec::new()).unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(token_of(&req(&format!("holzfile://localhost/{token}"))).as_deref(), Some(token));
        assert_eq!(token_of(&req(&format!("http://holzfile.localhost/{token}"))).as_deref(), Some(token));
        for bad in ["holzfile://localhost/", "holzfile://localhost/../etc/passwd", "holzfile://localhost/abc", "holzfile://localhost/0123456789abcdef0123456789abcdeg", "holzfile://localhost/0123456789abcdef0123456789abcdef/x"] {
            assert!(token_of(&req(bad)).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_url_names_the_scheme_the_platform_serves() {
        let url = url_of("abc");
        if cfg!(any(windows, target_os = "android")) {
            assert_eq!(url, "http://holzfile.localhost/abc");
        } else {
            assert_eq!(url, "holzfile://localhost/abc");
        }
    }
}
