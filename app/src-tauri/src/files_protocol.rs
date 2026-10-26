//! The `holzfile` URI scheme: how an image, a video, audio or a PDF is shown without loading the whole file into the
//! page and without a `blob:` URL (which the policy does not allow).
//!
//! A URL is `holzfile://localhost/<token>` (on Windows and Android `http://holzfile.localhost/<token>`). The token is
//! 128 random bits that `file_grant` made for one file; nothing else is served, there is no path and no directory, so a
//! page cannot ask for a file it was not given. Range requests are answered with `206`, in pieces of at most
//! [`CHUNK`] bytes, so a video starts and seeks at once. Every response says `nosniff`, `no-store` and a CSP of
//! `sandbox`, so even a file opened by itself could run nothing.

use tauri::http::{self, Method, Request, Response, StatusCode, header};

use holzwave_host::ranges::{Range, parse_range};

use holzwave_host::state::AppCore;

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

    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target};
    use holzwave_host::windows::{CursorSpot, Place};

    /// A scratch directory that goes away with the value.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-app-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("scratch dir");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A sink that tells no webview anything and opens no window.
    struct Quiet;

    impl Sink for Quiet {
        fn send(&self, _target: Target, _out: Out) {}

        fn close_window(&self, _label: &str) {}

        fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
            Ok(())
        }

        fn cursor_spot(&self, _source: &str) -> CursorSpot {
            CursorSpot::Unknown
        }
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

    #[test]
    fn a_granted_file_is_served_in_ranges_and_nothing_else_is() {
        use tauri::http::{Method, Request, StatusCode, header};
        let dir = Scratch::new("serve");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        png.extend((0..100u8).collect::<Vec<_>>());
        let file = dir.0.join("a.png");
        std::fs::write(&file, &png).unwrap();
        std::fs::write(dir.0.join("secret.txt"), "not shown").unwrap();
        let core = AppCore::new(dir.0.clone(), Arc::new(Quiet), ShellSpec::default());

        let grant = core.file_grant("local", file.to_str().unwrap()).unwrap();
        assert_eq!((grant.mime.as_str(), grant.size), ("image/png", png.len() as u64));
        assert!(core.file_grant("local", dir.0.join("secret.txt").to_str().unwrap()).is_err(), "text is not granted");
        let request = |method: Method, token: &str, range: Option<&str>| {
            let mut b = Request::builder().method(method).uri(format!("holzfile://localhost/{token}"));
            if let Some(r) = range {
                b = b.header(header::RANGE, r);
            }
            b.body(Vec::new()).unwrap()
        };

        let whole = serve(&core, &request(Method::GET, &grant.token, None));
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(whole.body(), &png);
        assert_eq!(whole.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(whole.headers()["X-Content-Type-Options"], "nosniff");
        assert_eq!(whole.headers()["Content-Security-Policy"], "default-src 'none'; sandbox");

        let part = serve(&core, &request(Method::GET, &grant.token, Some("bytes=16-19")));
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.body(), &png[16..20]);
        assert_eq!(part.headers()[header::CONTENT_RANGE], format!("bytes 16-19/{}", png.len()));
        let tail = serve(&core, &request(Method::GET, &grant.token, Some("bytes=-4")));
        assert_eq!(tail.body(), &png[png.len() - 4..]);
        let past = serve(&core, &request(Method::GET, &grant.token, Some("bytes=9999-")));
        assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);

        let head = serve(&core, &request(Method::HEAD, &grant.token, None));
        assert_eq!((head.status(), head.body().len()), (StatusCode::OK, 0));
        assert_eq!(head.headers()[header::CONTENT_LENGTH], png.len().to_string());
        assert_eq!(serve(&core, &request(Method::POST, &grant.token, None)).status(), StatusCode::METHOD_NOT_ALLOWED);

        // A token nobody was given, and a path that is not a token, read nothing.
        assert_eq!(serve(&core, &request(Method::GET, &"0".repeat(32), None)).status(), StatusCode::NOT_FOUND);
        let by_path = Request::builder().uri("holzfile://localhost/secret.txt").body(Vec::new()).unwrap();
        assert_eq!(serve(&core, &by_path).status(), StatusCode::NOT_FOUND);

        // After the preview is gone the URL stops working.
        core.file_revoke(&grant.token);
        assert_eq!(serve(&core, &request(Method::GET, &grant.token, None)).status(), StatusCode::NOT_FOUND);
    }
}
