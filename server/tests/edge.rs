//! The page and the listener, over real sockets: the embedded page with and without a session, the asset rules, the accept
//! loop under slow and abusive clients, the proxy mode, the orderly shutdown and the health socket.
//!
//! The page must be built first (`npm --prefix app run build`): the tests compare the answers with the files of `app/dist`.

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;

use edge_tls::{Request, dist, start};
use rustls::ProtocolVersion;

// ---------------------------------------------------------------------------------------------------------------------
// Tracer: the sign-in page, then the application, over a real TLS connection
// ---------------------------------------------------------------------------------------------------------------------

/// The line the SUMMARY quotes for one request of the tracer.
fn show(what: &str, reply: &edge_tls::Reply) {
    println!(
        "Tracer: {what}: {} {} cache={} etag={} len={}",
        reply.status,
        reply.header("content-type").unwrap_or("-"),
        reply.header("cache-control").unwrap_or("-"),
        reply.header("etag").is_some(),
        reply.body.len()
    );
}

#[tokio::test]
async fn tracer_the_router_alone_serves_the_sign_in_page_and_refuses_the_script() {
    // No socket: the parts of the page mounted on the router of the application layer.
    let app = edge::TestApp::with_parts(holzwave_server::http::embed::parts());
    let page = app.send(app.req(axum::http::Method::GET, "/")).await;
    assert_eq!(page.status, axum::http::StatusCode::OK, "the sign-in page is served");
    assert_eq!(page.body, dist("login.html"));
    let script = app.send(app.req(axum::http::Method::GET, "/main.js")).await;
    assert_eq!(script.status, axum::http::StatusCode::UNAUTHORIZED, "the application script needs a session");
}

#[tokio::test]
async fn tracer_sign_in_page_is_served_without_a_session() {
    let server = start().await;
    let reply = server.get("/", None).await;
    show("GET / without a session", &reply);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(reply.body, dist("login.html"), "the body is the sign-in page of the build");
    assert_eq!(reply.header("cache-control"), Some("no-store"), "an HTML answer is never cached");
    for name in ["content-security-policy", "strict-transport-security", "x-content-type-options", "referrer-policy"] {
        assert!(reply.header(name).is_some(), "the fixed security header {name} is on the page");
    }
    assert!(reply.header("set-cookie").is_none(), "serving the page sets no cookie");
}

#[tokio::test]
async fn tracer_a_script_the_sign_in_page_needs_has_a_type_and_a_tag() {
    let server = start().await;
    let reply = server.get("/login.js", None).await;
    show("GET /login.js without a session", &reply);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("content-type"), Some("text/javascript; charset=utf-8"));
    assert!(reply.header("etag").is_some_and(|tag| tag.starts_with('"') && tag.ends_with('"')), "a strong tag in quotes");
    assert_eq!(reply.body, dist("login.js"));
}

#[tokio::test]
async fn tracer_the_application_script_is_refused_without_a_session() {
    let server = start().await;
    let reply = server.get("/main.js", None).await;
    show("GET /main.js without a session", &reply);
    assert_eq!(reply.status, 401);
    assert_eq!(reply.json(), serde_json::json!({ "code": "invalid" }));
    assert!(reply.header("set-cookie").is_none());
}

#[tokio::test]
async fn tracer_the_application_page_and_its_script_are_served_with_a_session() {
    let server = start().await;
    server.test.create_account("alice", edge_role());
    let alice = server.test.sign_in("alice");

    let page = server.get("/", Some(&alice.cookie)).await;
    show("GET / with a session", &page);
    assert_eq!(page.status, 200);
    assert_eq!(page.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert_eq!(page.body, dist("index.html"), "a signed-in person gets the application page");

    let script = server.get("/main.js", Some(&alice.cookie)).await;
    show("GET /main.js with a session", &script);
    assert_eq!(script.status, 200);
    assert_eq!(script.header("content-type"), Some("text/javascript; charset=utf-8"));
    assert_eq!(script.body, dist("main.js"));
}

#[tokio::test]
async fn tracer_an_unknown_path_is_404_with_and_without_a_session() {
    let server = start().await;
    server.test.create_account("alice", edge_role());
    let alice = server.test.sign_in("alice");
    let without = server.get("/nothing.js", None).await;
    let with = server.get("/nothing.js", Some(&alice.cookie)).await;
    show("GET /nothing.js without a session", &without);
    show("GET /nothing.js with a session", &with);
    assert_eq!((without.status, with.status), (404, 404));
    assert_eq!(without.body, with.body, "the answer does not say whether a session exists");
    assert_eq!(without.json(), serde_json::json!({ "code": "not_found" }));
}

#[tokio::test]
async fn tracer_the_session_is_tls_13_and_http_11_is_negotiated_when_offered_alone() {
    let server = start().await;
    let stream = server.tls(&[b"http/1.1"]).await;
    let (_, connection) = stream.get_ref();
    assert_eq!(connection.protocol_version(), Some(ProtocolVersion::TLSv1_3));
    assert_eq!(connection.alpn_protocol(), Some(&b"http/1.1"[..]));
}

#[tokio::test]
async fn tracer_plain_http_sent_to_the_tls_port_gets_no_http_bytes() {
    let server = start().await;
    let mut conn = edge_tls::Conn::new(server.tcp().await);
    conn.send(&Request::get("/").close().bytes()).await;
    let answer = conn.rest().await;
    assert!(!answer.starts_with(b"HTTP/"), "no byte of HTTP is served before the handshake completes");
    assert!(!String::from_utf8_lossy(&answer).contains("login"), "the page is not served in the clear");
}

fn edge_role() -> holzwave_server::store::Role {
    holzwave_server::store::Role::Member
}

// ---------------------------------------------------------------------------------------------------------------------
// The asset rules: the public list against the real build, types, caching and hostile paths
// ---------------------------------------------------------------------------------------------------------------------

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Bytes;
use edge_tls::{Reply, Started};
use holzwave_server::http::embed::{self, Assets, PUBLIC_FILES, PUBLIC_PREFIX};
use sha2::{Digest, Sha256};

/// A signed-in `alice` of a started listener.
fn alice(server: &Started) -> String {
    server.test.create_account("alice", edge_role());
    server.test.sign_in("alice").cookie
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Every file of the build folder as a path with a leading slash, found on disk.
fn built_files() -> Vec<String> {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read the build folder") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let relative = path.strip_prefix(base).expect("inside the folder");
                out.push(format!("/{}", relative.to_string_lossy()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&edge_tls::dist_dir(), &edge_tls::dist_dir(), &mut out);
    out.sort();
    out
}

fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, extension)| extension)
}

// ---- the list against the closure of the built sign-in script --------------------------------------------------------

/// The text between quotes that follows each of `markers` in `source`.
fn quoted_after(source: &str, markers: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for marker in markers {
        let mut rest = source;
        while let Some(at) = rest.find(marker) {
            rest = &rest[at + marker.len()..];
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            if let Some(end) = rest[1..].find(quote) {
                found.push(rest[1..1 + end].to_owned());
            }
        }
    }
    found
}

/// `spec` resolved against the directory of `from` (both with a leading slash), with dot segments removed.
fn resolve(from: &str, spec: &str) -> String {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for piece in spec.split('/') {
        match piece {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if joined.starts_with('/') { joined } else { format!("/{joined}") }
}

fn relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

/// Everything the sign-in page uses, found by following the built files: the script and the styles the page names, the
/// modules the script imports (and theirs), and the files the styles import or take with `url()`.
fn sign_in_closure() -> BTreeSet<String> {
    let read = |path: &str| String::from_utf8(dist(&path[1..])).expect("a text file");
    let page = read("/login.html");
    let mut todo: Vec<String> = quoted_after(&page, &["src=", "href="])
        .into_iter()
        .filter(|spec| relative(spec))
        .map(|spec| resolve("/login.html", &spec))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(path) = todo.pop() {
        if !seen.insert(path.clone()) || path.starts_with(PUBLIC_PREFIX) {
            continue;
        }
        let source = read(&path);
        let specs = match extension(&path) {
            "js" => quoted_after(&source, &["from ", "import ", "import("]),
            "css" => {
                let mut specs = quoted_after(&source, &["@import ", "url(", "@import url("]);
                // A `url()` without quotes.
                for piece in source.split("url(").skip(1) {
                    if let Some(end) = piece.find(')') {
                        specs.push(piece[..end].trim_matches(|c| c == '"' || c == '\'').to_owned());
                    }
                }
                specs
            }
            _ => Vec::new(),
        };
        for spec in specs.into_iter().filter(|s| relative(s)) {
            todo.push(resolve(&path, &spec));
        }
    }
    seen
}

/// The ways a list can disagree with a closure and a build: nothing for a list that is right.
fn disagreements(closure: &BTreeSet<String>, list: &[&str], built: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut problems = Vec::new();
    for path in closure {
        if !(list.contains(&path.as_str()) || path.starts_with(PUBLIC_PREFIX)) {
            problems.push(format!("the sign-in page uses {path} and the list does not name it"));
        }
    }
    for entry in list {
        let seen_by_closure = closure.contains(*entry) || *entry == "/login.html" || *entry == "/favicon.ico";
        if !seen_by_closure {
            problems.push(format!("the list names {entry} and the sign-in page does not use it"));
        }
        if *entry != "/favicon.ico" && !built(entry) {
            problems.push(format!("the list names {entry} and the build does not produce it"));
        }
    }
    problems
}

#[test]
fn list_the_closure_of_the_built_sign_in_script_equals_the_public_list() {
    let closure = sign_in_closure();
    let on_disk: BTreeSet<String> = built_files().into_iter().collect();
    println!("List: closure = {:?}", closure.iter().filter(|p| !p.starts_with(PUBLIC_PREFIX)).collect::<Vec<_>>());
    println!("List: fonts in the closure = {}", closure.iter().filter(|p| p.starts_with(PUBLIC_PREFIX)).count());
    println!("List: written = {PUBLIC_FILES:?} and everything under {PUBLIC_PREFIX}");
    let problems = disagreements(&closure, PUBLIC_FILES, &|p| on_disk.contains(p));
    assert!(problems.is_empty(), "the public list and the built sign-in page disagree: {problems:#?}");
    assert!(closure.contains("/login.js") && closure.contains("/bridge.js"), "the walker found the script and a module of it");
}

#[test]
fn list_the_check_finds_a_module_that_is_not_on_the_list_and_an_entry_the_build_does_not_have() {
    let closure: BTreeSet<String> = ["/login.js", "/bridge.js", "/new-module.js"].map(String::from).into();
    let problems = disagreements(&closure, &["/login.html", "/login.js", "/bridge.js", "/stale.js"], &|p| p != "/stale.js");
    assert!(problems.iter().any(|p| p.contains("/new-module.js") && p.contains("does not name it")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("/stale.js") && p.contains("does not use it")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("/stale.js") && p.contains("does not produce it")), "{problems:?}");
}

#[test]
fn list_every_public_file_the_list_names_is_embedded_except_the_favicon() {
    let table = Assets::embedded();
    for path in PUBLIC_FILES.iter().filter(|p| **p != "/favicon.ico") {
        assert!(table.get(path).is_some(), "the list names {path} and the server does not embed it");
    }
    assert!(PUBLIC_FILES.iter().filter(|p| extension(p) == "html").eq(["/login.html"].iter()), "the sign-in page is the only HTML file");
}

#[test]
fn list_the_prefix_rule_covers_the_fonts_and_nothing_next_to_them() {
    assert!(embed::is_public("/fonts/JetBrainsMono-Regular.woff2"));
    assert!(embed::is_public("/fonts/OFL.txt"));
    for path in ["/fonts/", "/fonts", "/fontsx/a.woff2", "/font/a.woff2", "/Fonts/a.woff2", "/index.html", "/main.js", "/vendor/x.js"] {
        assert!(!embed::is_public(path), "{path} is not public");
    }
}

// ---- types -----------------------------------------------------------------------------------------------------------

#[test]
fn types_the_table_maps_the_nine_extensions_to_their_content_types() {
    let expected = [
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
    assert_eq!(embed::EXTENSIONS, expected.as_slice());
}

#[test]
fn types_a_file_with_another_extension_is_neither_served_nor_listed() {
    let body = Bytes::from_static(b"x");
    let table = Assets::from_files([
        ("main.js".to_owned(), body.clone()),
        ("main.js.map".to_owned(), body.clone()),
        ("vendor/pdf.min.mjs".to_owned(), body.clone()),
        ("vendor/cmaps/H.bcmap".to_owned(), body.clone()),
        ("LICENSE".to_owned(), body.clone()),
        ("MAIN.JS".to_owned(), body.clone()),
    ]);
    assert_eq!(table.paths().collect::<Vec<_>>(), ["/main.js"], "only the file with an allowed extension is in the table");
    assert_eq!(table.skipped(), 5, "the others are counted");
    assert!(table.get("/main.js.map").is_none());
}

#[test]
fn types_a_name_with_an_odd_character_or_a_dot_segment_is_left_out() {
    let body = Bytes::from_static(b"x");
    let table = Assets::from_files(
        ["a{b}.js", "a b.js", "a:b.js", "a*.js", "../x.js", "a/../b.js", "a//b.js", "a\\b.js", "ä.js", "ok-1_2.js", "dir/ok.css"]
            .map(|name| (name.to_owned(), body.clone())),
    );
    assert_eq!(table.paths().collect::<Vec<_>>(), ["/dir/ok.css", "/ok-1_2.js"]);
    assert_eq!(table.skipped(), 9);
}

#[test]
fn types_the_embedded_page_holds_only_allowed_extensions_and_the_table_agrees_with_the_disk() {
    let table = Assets::embedded();
    let allowed: BTreeSet<&str> = embed::EXTENSIONS.iter().map(|(extension, _)| *extension).collect();
    let mut per_extension: BTreeMap<String, usize> = BTreeMap::new();
    for path in table.paths() {
        assert!(allowed.contains(extension(path)), "{path} is embedded and its extension is not on the list");
        *per_extension.entry(extension(path).to_owned()).or_default() += 1;
    }
    let on_disk = built_files();
    let should_be: Vec<&String> = on_disk.iter().filter(|p| allowed.contains(extension(p))).collect();
    assert_eq!(table.len(), should_be.len(), "the table holds every built file with an allowed extension");
    assert_eq!(table.skipped(), on_disk.len() - should_be.len());
    println!("Types: built files = {}, embedded = {}, left out = {}", on_disk.len(), table.len(), table.skipped());
    println!("Types: per extension (embedded) = {per_extension:?}");
}

#[test]
fn types_the_build_output_holds_no_source_map() {
    let maps: Vec<String> = built_files().into_iter().filter(|p| p.ends_with(".map")).collect();
    assert!(maps.is_empty(), "the build wrote source maps, which the server never serves: {maps:?}");
}

#[tokio::test]
async fn types_a_built_file_with_another_extension_answers_404_even_with_a_session() {
    let server = start().await;
    let cookie = alice(&server);
    let others: Vec<String> = built_files().into_iter().filter(|p| !embed::EXTENSIONS.iter().any(|(e, _)| *e == extension(p))).collect();
    assert!(!others.is_empty(), "the build holds files the closed list leaves out (PDF.js data, licences)");
    let mut conn = server.h1().await;
    for path in others.iter().take(12) {
        conn.send(&Request::get(path).cookie(&cookie).bytes()).await;
        let reply = conn.reply(false).await.expect("an answer");
        assert_eq!(reply.status, 404, "{path} is not served");
    }
}

#[tokio::test]
async fn types_every_answer_has_the_type_of_its_extension_and_nosniff() {
    let server = start().await;
    let cookie = alice(&server);
    let mut conn = server.h1().await;
    for path in Assets::embedded().paths().take(60) {
        conn.send(&Request::get(path).cookie(&cookie).bytes()).await;
        let reply = conn.reply(false).await.expect("an answer");
        let expected = embed::EXTENSIONS.iter().find(|(e, _)| *e == extension(path)).map(|(_, t)| *t);
        assert_eq!(reply.header("content-type"), expected, "the type of {path}");
        assert_eq!(reply.header("x-content-type-options"), Some("nosniff"), "nosniff on {path}");
    }
}

// ---- public against protected ----------------------------------------------------------------------------------------

#[tokio::test]
async fn walk_every_public_file_answers_200_without_a_session_and_every_other_file_401() {
    let server = start().await;
    let mut conn = server.h1().await;
    let mut seen: BTreeMap<u16, usize> = BTreeMap::new();
    let (mut public, mut protected) = (0, 0);
    for path in Assets::embedded().paths() {
        conn.send(&Request::get(path).bytes()).await;
        let reply = conn.reply(false).await.expect("an answer");
        *seen.entry(reply.status).or_default() += 1;
        if embed::is_public(path) {
            public += 1;
            assert_eq!(reply.status, 200, "{path} is public");
        } else {
            protected += 1;
            assert_eq!(reply.status, 401, "{path} needs a session");
            assert_eq!(reply.json(), serde_json::json!({ "code": "invalid" }), "{path}");
        }
    }
    println!("Walk: without a session: public = {public}, protected = {protected}, statuses = {seen:?}");
    assert!(public > 10 && protected > 50, "the walk covered the page");
}

#[tokio::test]
async fn walk_every_embedded_file_answers_200_with_its_bytes_for_a_signed_in_person() {
    let server = start().await;
    let cookie = alice(&server);
    let mut conn = server.h1().await;
    let mut count = 0;
    for path in Assets::embedded().paths() {
        conn.send(&Request::get(path).cookie(&cookie).bytes()).await;
        let reply = conn.reply(false).await.expect("an answer");
        assert_eq!(reply.status, 200, "{path} with a session");
        assert_eq!(reply.body, dist(&path[1..]), "the bytes of {path}");
        count += 1;
    }
    println!("Walk: with a session: {count} files, all 200 with the bytes of the build");
}

#[tokio::test]
async fn walk_index_html_needs_a_session_and_login_html_does_not() {
    let server = start().await;
    let cookie = alice(&server);
    assert_eq!(server.get("/index.html", None).await.status, 401);
    let with = server.get("/index.html", Some(&cookie)).await;
    assert_eq!((with.status, with.body), (200, dist("index.html")));
    let login = server.get("/login.html", Some(&cookie)).await;
    assert_eq!((login.status, login.body), (200, dist("login.html")), "the sign-in page is public even with a session");
    let root = server.get("/", Some(&cookie)).await;
    assert_eq!(root.body, dist("index.html"));
}

#[tokio::test]
async fn walk_a_file_outside_the_set_is_404_with_and_without_a_session() {
    let server = start().await;
    let cookie = alice(&server);
    for path in ["/nothing.js", "/index.htm", "/main.ts", "/src/main.ts", "/Cargo.toml", "/server/Cargo.toml", "/.env", "/vendor", "/fonts/nothing.woff2"] {
        let without = server.get(path, None).await;
        let with = server.get(path, Some(&cookie)).await;
        assert_eq!((without.status, with.status), (404, 404), "{path}");
        assert_eq!(without.json(), serde_json::json!({ "code": "not_found" }), "{path} says nothing more");
        assert_eq!(without.body, with.body, "{path}: the answer does not tell whether a session exists");
    }
}

#[tokio::test]
async fn walk_a_conditional_request_does_not_get_past_the_session_check() {
    let server = start().await;
    let tag = Assets::embedded().get("/main.js").expect("main.js").etag.clone();
    let reply = server.send(Request::get("/main.js").header("If-None-Match", &tag).close()).await;
    assert_eq!(reply.status, 401, "a request that knows the tag still needs a session");
}

#[tokio::test]
async fn walk_the_page_is_served_from_memory_not_from_the_disk() {
    // A table whose file exists nowhere on the disk: the answer can only come from memory.
    let table: &'static Assets =
        Box::leak(Box::new(Assets::from_files([("memory-only.js".to_owned(), Bytes::from_static(b"export {};"))])));
    let server = edge_tls::start_with(edge_tls::Opts { page: false, extra: embed::parts_of(table), ..edge_tls::Opts::default() }).await;
    let reply = server.send(Request::get("/memory-only.js").close()).await;
    assert_eq!(reply.status, 401, "a file that is not on the public list needs a session");
    let cookie = alice(&server);
    let reply = server.get("/memory-only.js", Some(&cookie)).await;
    assert_eq!((reply.status, reply.body), (200, b"export {};".to_vec()));
}

#[test]
fn walk_the_embed_code_does_not_touch_the_file_system_for_a_request() {
    let source = include_str!("../src/http/embed.rs");
    for api in ["std::fs", "tokio::fs", "File::open", "read_dir", "canonicalize", "read_to_string", "Path::new"] {
        assert!(!source.contains(api), "embed.rs must answer from the table, not through {api}");
    }
    for api in ["percent_decode", "decode_utf8", "urlencoding"] {
        assert!(!source.contains(api), "embed.rs must not decode a path ({api})");
    }
}

// ---- caching ---------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn cache_html_is_never_stored_and_has_no_tag() {
    let server = start().await;
    let cookie = alice(&server);
    for (path, session) in [("/", false), ("/", true), ("/login.html", false), ("/index.html", true)] {
        let reply = server.get(path, session.then_some(cookie.as_str())).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.header("cache-control"), Some("no-store"), "{path} session={session}");
        assert!(reply.header("etag").is_none(), "{path} has no tag");
    }
}

#[tokio::test]
async fn cache_other_assets_have_no_cache_and_a_strong_tag_of_the_content_hash() {
    let server = start().await;
    for path in ["/login.js", "/login.css", "/app.css", "/fonts/JetBrainsMono-Regular.woff2", "/fonts/OFL.txt"] {
        let reply = server.get(path, None).await;
        assert_eq!(reply.status, 200, "{path}");
        assert_eq!(reply.header("cache-control"), Some("no-cache"), "{path}");
        let tag = reply.header("etag").expect("a tag");
        let digits = tag.strip_prefix('"').and_then(|t| t.strip_suffix('"')).expect("the tag is in quotes");
        assert_eq!(digits.len(), 64, "{path}: 64 hex digits");
        assert_eq!(digits, sha256_hex(&reply.body), "{path}: the tag is the SHA-256 of the body");
        assert!(!tag.starts_with("W/"), "a strong tag");
    }
}

#[tokio::test]
async fn cache_a_matching_if_none_match_answers_304_with_the_same_headers_and_no_body() {
    let server = start().await;
    let full = server.get("/login.js", None).await;
    let tag = full.header("etag").expect("a tag").to_owned();
    let mut conn = server.h1().await;
    conn.send(&Request::get("/login.js").header("If-None-Match", &tag).bytes()).await;
    let reply = conn.reply(false).await.expect("an answer");
    assert_eq!(reply.status, 304);
    assert!(reply.body.is_empty(), "a 304 has no body");
    assert_eq!(reply.header("etag"), Some(tag.as_str()));
    assert_eq!(reply.header("cache-control"), Some("no-cache"));
    assert_eq!(reply.header("content-type"), full.header("content-type"));
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    // The connection is still good for the next request.
    conn.send(&Request::get("/login.css").close().bytes()).await;
    assert_eq!(conn.reply(false).await.expect("the next answer").status, 200);
}

#[tokio::test]
async fn cache_if_none_match_takes_a_weak_tag_a_list_and_a_star() {
    let server = start().await;
    let tag = server.get("/login.js", None).await.header("etag").expect("a tag").to_owned();
    for (value, expected) in [
        (format!("W/{tag}"), 304),
        (format!("\"0\", {tag}, \"1\""), 304),
        ("*".to_owned(), 304),
        ("\"0000\"".to_owned(), 200),
        (tag.replace('"', ""), 200),
        (format!("{tag}x"), 200),
        (String::new(), 200),
    ] {
        let reply = server.send(Request::get("/login.js").header("If-None-Match", &value).close()).await;
        assert_eq!(reply.status, expected, "If-None-Match: {value}");
        assert_eq!(reply.body.is_empty(), expected == 304, "If-None-Match: {value}");
    }
}

#[tokio::test]
async fn cache_a_different_tag_gets_the_whole_file() {
    let server = start().await;
    let reply = server.send(Request::get("/login.js").header("If-None-Match", "\"somethingelse\"").close()).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, dist("login.js"));
}

#[tokio::test]
async fn cache_head_has_the_headers_of_get_and_no_body() {
    let server = start().await;
    for path in ["/login.js", "/login.html", "/"] {
        let get = server.get(path, None).await;
        let head = server.send(Request::new("HEAD", path).close()).await;
        assert_eq!(head.status, 200, "{path}");
        assert!(head.body.is_empty(), "{path}: no body");
        for name in ["content-type", "cache-control", "etag", "content-length", "content-security-policy"] {
            assert_eq!(head.header(name), get.header(name), "{path}: {name}");
        }
    }
}

#[tokio::test]
async fn cache_a_range_header_is_ignored() {
    let server = start().await;
    let reply = server.send(Request::get("/login.js").header("Range", "bytes=0-10").close()).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, dist("login.js"), "the whole file");
    assert!(reply.header("content-range").is_none() && reply.header("accept-ranges").is_none());
}

#[tokio::test]
async fn cache_no_answer_of_the_page_sets_a_cookie() {
    let server = start().await;
    let cookie = alice(&server);
    let mut conn = server.h1().await;
    let tag = server.get("/login.js", None).await.header("etag").expect("a tag").to_owned();
    for request in [
        Request::get("/"),
        Request::get("/login.js"),
        Request::get("/login.js").header("If-None-Match", &tag),
        Request::get("/main.js"),
        Request::get("/main.js").cookie(&cookie),
        Request::get("/nothing"),
        Request::get("/").cookie(&cookie),
    ] {
        conn.send(&request.bytes()).await;
        let reply = conn.reply(false).await.expect("an answer");
        assert!(reply.header("set-cookie").is_none(), "{} {:?}", reply.status, request.target);
    }
}

// ---- hostile paths ---------------------------------------------------------------------------------------------------

/// The path as a request line carries it, answered with and without a session.
async fn both(server: &Started, cookie: &str, target: &str) -> (Reply, Reply) {
    (server.send(Request::get(target).close()).await, server.send(Request::get(target).cookie(cookie).close()).await)
}

/// A target that must never reach anything: 404 with the standard body, with and without a session.
async fn assert_not_found(server: &Started, cookie: &str, target: &str) {
    let (without, with) = both(server, cookie, target).await;
    for (reply, how) in [(&without, "without a session"), (&with, "with a session")] {
        assert_eq!(reply.status, 404, "{target} {how}");
        assert_eq!(reply.json(), serde_json::json!({ "code": "not_found" }), "{target} {how}: the standard 404 body");
    }
    let shown: String = target.chars().take(40).collect();
    println!("Paths: {shown:?} ({} bytes) -> {} / {}", target.len(), without.status, with.status);
}

#[tokio::test]
async fn paths_dot_segments_and_encoded_dots_are_404() {
    let server = start().await;
    let cookie = alice(&server);
    for target in ["/../Cargo.toml", "/%2e%2e/Cargo.toml", "/%2E%2E%2F", "/..%2f", "/.%2e/", "/fonts/../main.js", "/./main.js"] {
        assert_not_found(&server, &cookie, target).await;
    }
}

#[tokio::test]
async fn paths_slashes_case_and_directory_names_are_404() {
    let server = start().await;
    let cookie = alice(&server);
    for target in ["/main.js/", "//main.js", "/MAIN.JS", "/Main.js", "/fonts/", "/fonts", "/vendor/", "///"] {
        assert_not_found(&server, &cookie, target).await;
    }
}

#[tokio::test]
async fn paths_encoded_slashes_nul_and_backslashes_are_404() {
    let server = start().await;
    let cookie = alice(&server);
    for target in ["/main.js%00", "/main.js%2500", "/%5cmain.js", "/%5Cmain.js", "/%2fmain.js", "/fonts%2fOFL.txt", "/main%2ejs", "/%6dain.js"] {
        assert_not_found(&server, &cookie, target).await;
    }
}

#[tokio::test]
async fn paths_a_raw_backslash_is_404() {
    let server = start().await;
    let cookie = alice(&server);
    for target in ["/\\main.js", "/fonts\\OFL.txt", "/..\\Cargo.toml"] {
        let (without, with) = both(&server, &cookie, target).await;
        println!("Paths: {target:?} -> {} / {}", without.status, with.status);
        for reply in [without, with] {
            assert_eq!(reply.status, 404, "{target}");
            assert_eq!(reply.json(), serde_json::json!({ "code": "not_found" }), "{target}: the standard 404 body");
        }
    }
}

#[tokio::test]
async fn paths_a_very_long_path_is_404() {
    let server = start().await;
    let cookie = alice(&server);
    assert_not_found(&server, &cookie, &format!("/{}", "a".repeat(8 * 1024))).await;
    assert_not_found(&server, &cookie, &format!("/{}.js", "a/".repeat(2000))).await;
}

#[tokio::test]
async fn paths_the_query_string_is_ignored() {
    let server = start().await;
    let cookie = alice(&server);
    let without = server.get("/login.js?x=1", None).await;
    assert_eq!((without.status, without.body), (200, dist("login.js")));
    let with = server.get("/main.js?x=1&y=%2e%2e", Some(&cookie)).await;
    assert_eq!((with.status, with.body), (200, dist("main.js")));
    let long = server.get(&format!("/login.js?{}", "q".repeat(4000)), None).await;
    assert_eq!(long.status, 200, "a long query is still a query");
    println!("Paths: \"/main.js?x=1\" -> 200 (the query is ignored)");
}

// ---- the check of the build ------------------------------------------------------------------------------------------

#[test]
fn verify_refuses_a_build_without_the_application_page_or_the_sign_in_page() {
    let sentence = "The page is not built. Run npm --prefix app run build before building the server.";
    for names in [vec![], vec!["main.js"], vec!["index.html"], vec!["login.html"], vec!["index.html.map", "login.html.bak"], vec!["Index.html", "Login.html"]] {
        let error = embed::check_names(&names).expect_err(&format!("{names:?} is not a page"));
        assert_eq!(error.to_string(), sentence);
    }
    embed::check_names(&["index.html", "login.html"]).expect("both pages");
    embed::check_names(&["main.js", "fonts/a.woff2", "login.html", "index.html"]).expect("both pages among others");
}

#[test]
fn verify_accepts_the_page_this_build_embeds() {
    embed::verify().expect("the page is built");
}

// ---------------------------------------------------------------------------------------------------------------------
// The accept loop under pressure, proxy mode, shutdown, the health socket and `healthcheck`
// ---------------------------------------------------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use edge_tls::{Opts, RawH2, Read, ShortDir, quick, start_with};
use holzwave_server::http::app::{Shutdown, Timing, TransportKind};
use holzwave_server::http::health::{self, Health, Readiness};
use holzwave_server::http::router::{Access, ClientAddr, Part};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

type Hits = Arc<AtomicUsize>;

async fn slow(State(hits): State<Hits>, Query(query): Query<HashMap<String, String>>) -> &'static str {
    hits.fetch_add(1, Ordering::SeqCst);
    let ms = query.get("ms").and_then(|v| v.parse().ok()).unwrap_or(1000);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    "slow"
}

async fn hit(State(hits): State<Hits>) -> &'static str {
    hits.fetch_add(1, Ordering::SeqCst);
    "hit"
}

async fn panics() -> &'static str {
    panic!("a handler that panics");
}

async fn echo(Extension(client): Extension<ClientAddr>, kind: Option<Extension<TransportKind>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "client": client.0.to_string(), "transport": kind.map(|Extension(k)| format!("{k:?}")) }))
}

async fn until_signal(Extension(shutdown): Extension<Shutdown>) -> &'static str {
    shutdown.wait().await;
    "closing"
}

async fn is_signalled(Extension(shutdown): Extension<Shutdown>) -> &'static str {
    if shutdown.is_triggered() { "yes" } else { "no" }
}

async fn body_length(body: axum::body::Bytes) -> String {
    body.len().to_string()
}

/// Probe routes for the listener tests; `hits` counts the requests that reached a handler.
fn routes(hits: &Hits) -> Vec<Part> {
    let counted = Router::new().route("/t/slow", get(slow)).route("/t/hit", get(hit)).with_state(hits.clone());
    let plain = Router::new()
        .route("/t/panic", get(panics))
        .route("/t/echo", get(echo))
        .route("/t/signal", get(until_signal))
        .route("/t/signalled", get(is_signalled))
        .route("/t/body", post(body_length));
    vec![Part { access: Access::Public, router: counted }, Part { access: Access::Public, router: plain }]
}

async fn quick_server(timing: Timing) -> (Started, Hits) {
    let hits: Hits = Arc::default();
    let server = start_with(Opts { extra: routes(&hits), timing: Some(timing), ..Opts::default() }).await;
    (server, hits)
}

/// Polls `condition` until it holds or `within` has passed.
async fn eventually(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    condition()
}

/// Reads until the peer ends the stream (or fails), for at most `limit`. `None` when it did not end in time; otherwise the
/// bytes that arrived before the end.
async fn until_closed<S: AsyncRead + Unpin>(stream: &mut S, limit: Duration) -> Option<Vec<u8>> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let mut chunk = [0u8; 4096];
        match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Err(_) => return None,
            Ok(Ok(n)) if n > 0 => seen.extend_from_slice(&chunk[..n]),
            Ok(_) => return Some(seen),
        }
    }
}

// ---- slow and idle clients -------------------------------------------------------------------------------------------

#[tokio::test]
async fn slow_headers_a_byte_at_a_time_are_cut_at_the_header_time() {
    let (server, hits) = quick_server(quick()).await;
    let tls = server.tls(&[b"http/1.1"]).await;
    let (mut reader, mut writer) = tokio::io::split(tls);
    let began = Instant::now();
    tokio::spawn(async move {
        let _ = writer.write_all(b"GET /t/hit HTTP/1.1\r\nHost: hw.example.test\r\n").await;
        for byte in b"X-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".iter() {
            tokio::time::sleep(Duration::from_millis(80)).await;
            if writer.write_all(&[*byte]).await.is_err() {
                return;
            }
        }
    });
    let seen = until_closed(&mut reader, Duration::from_secs(5)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: header trickle cut after {took:?} (header time {:?})", quick().header);
    assert!(took >= quick().header.mul_f32(0.8), "not before the header time: {took:?}");
    assert!(took < quick().header + Duration::from_millis(1500), "not long after it: {took:?}");
    assert!(!seen.starts_with(b"HTTP/"), "no answer to half a request");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the request never reached a handler");
}

#[tokio::test]
async fn slow_a_tcp_connection_that_sends_nothing_is_cut_at_the_handshake_time() {
    let (server, _) = quick_server(quick()).await;
    let mut tcp = server.tcp().await;
    let began = Instant::now();
    let seen = until_closed(&mut tcp, Duration::from_secs(5)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: silent TCP connection cut after {took:?} (handshake time {:?})", quick().handshake);
    assert!(took >= quick().handshake.mul_f32(0.8) && took < quick().handshake + Duration::from_millis(1500), "{took:?}");
    assert!(seen.is_empty(), "nothing was sent to a peer that said nothing");
}

#[tokio::test]
async fn slow_a_handshake_that_trickles_is_cut_at_the_handshake_time() {
    let (server, _) = quick_server(quick()).await;
    let tcp = server.tcp().await;
    let (mut reader, mut writer) = tokio::io::split(tcp);
    let began = Instant::now();
    tokio::spawn(async move {
        // The start of a TLS record header, then one byte at a time.
        for byte in [0x16u8, 0x03, 0x01, 0x02, 0x00, 0x01, 0x00, 0x01, 0xfc, 0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00] {
            if writer.write_all(&[byte]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
    until_closed(&mut reader, Duration::from_secs(5)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: trickled handshake cut after {took:?}");
    assert!(took >= quick().handshake.mul_f32(0.8) && took < quick().handshake + Duration::from_millis(1500), "{took:?}");
}

#[tokio::test]
async fn slow_a_tls_connection_that_never_sends_a_request_is_cut_at_the_header_time() {
    let (server, _) = quick_server(quick()).await;
    let mut tls = server.tls(&[b"http/1.1"]).await;
    let began = Instant::now();
    until_closed(&mut tls, Duration::from_secs(5)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: TLS connection without a request cut after {took:?}");
    assert!(took >= quick().header.mul_f32(0.8) && took < quick().header + Duration::from_millis(1500), "{took:?}");
}

#[tokio::test]
async fn slow_in_proxy_mode_a_connection_that_sends_nothing_is_cut_at_the_header_time() {
    let hits: Hits = Arc::default();
    let server = start_with(Opts { extra: routes(&hits), timing: Some(quick()), trusted_proxies: Some("127.0.0.1/32"), ..Opts::default() }).await;
    let mut tcp = server.tcp().await;
    let began = Instant::now();
    until_closed(&mut tcp, Duration::from_secs(5)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: silent proxy connection cut after {took:?} (header time {:?})", quick().header);
    assert!(took >= quick().header.mul_f32(0.8) && took < quick().header + Duration::from_millis(1500), "{took:?}");
}

#[tokio::test]
async fn slow_an_idle_http_11_connection_is_closed_after_the_idle_time() {
    let (server, _) = quick_server(quick()).await;
    let mut conn = server.h1().await;
    conn.send(&Request::get("/login.css").bytes()).await;
    assert_eq!(conn.reply(false).await.expect("an answer").status, 200);
    let began = Instant::now();
    until_closed(&mut conn.stream, Duration::from_secs(6)).await.expect("the server closed the idle connection");
    let took = began.elapsed();
    println!("Slow: idle keep-alive connection closed after {took:?} (idle time {:?})", quick().idle);
    assert!(took >= quick().idle.mul_f32(0.8), "it waited for the idle time and not the header time: {took:?}");
    assert!(took < quick().idle + Duration::from_millis(2000), "{took:?}");
}

#[tokio::test]
async fn slow_a_connection_that_keeps_asking_stays_open_past_the_idle_time() {
    let (server, _) = quick_server(quick()).await;
    let mut conn = server.h1().await;
    let began = Instant::now();
    let mut asked = 0;
    while began.elapsed() < quick().idle * 2 {
        conn.send(&Request::get("/login.css").bytes()).await;
        assert_eq!(conn.reply(false).await.expect("the connection is still open").status, 200);
        asked += 1;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    println!("Slow: {asked} requests in {:?} on one connection (idle time {:?})", began.elapsed(), quick().idle);
    assert!(asked >= 6);
}

#[tokio::test]
async fn slow_a_body_that_stops_arriving_is_cut_after_the_idle_time() {
    let (server, hits) = quick_server(quick()).await;
    let tls = server.tls(&[b"http/1.1"]).await;
    let (mut reader, mut writer) = tokio::io::split(tls);
    let head = "POST /t/body HTTP/1.1\r\nHost: hw.example.test\r\nOrigin: https://hw.example.test\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n0123456789";
    writer.write_all(head.as_bytes()).await.expect("write");
    let began = Instant::now();
    until_closed(&mut reader, Duration::from_secs(6)).await.expect("the server cut the connection");
    let took = began.elapsed();
    println!("Slow: stalled body cut after {took:?} (idle time {:?})", quick().idle);
    assert!(took >= quick().idle.mul_f32(0.8) && took < quick().idle + Duration::from_millis(2000), "{took:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn slow_a_client_that_stops_reading_a_large_answer_is_cut_after_the_idle_time() {
    let (server, _) = quick_server(quick()).await;
    let tls = server.tls(&[b"http/1.1"]).await;
    let (mut reader, mut writer) = tokio::io::split(tls);
    // Ask for the largest public file many times over and never read: the answers fill every buffer.
    let big = "/fonts/Manrope-Variable-latin.woff2";
    let mut burst = Vec::new();
    for _ in 0..400 {
        burst.extend_from_slice(&Request::get(big).bytes());
    }
    let began = Instant::now();
    let _ = writer.write_all(&burst).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // The server is stuck writing; it must not be stuck for ever. Reading now drains what is buffered and then sees the end.
    let drained = until_closed(&mut reader, Duration::from_secs(8)).await;
    let took = began.elapsed();
    println!("Slow: a client that stopped reading was cut after {took:?}");
    assert!(drained.is_some(), "the connection ended");
}

// ---- admission -------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn admission_the_65th_connection_of_one_address_is_closed_before_any_read() {
    let server = start().await;
    let mut held = Vec::new();
    for _ in 0..64 {
        held.push(server.tcp().await);
    }
    assert!(eventually(Duration::from_secs(5), || server.admission.open_connections() == 64).await, "64 admitted");
    let mut extra = server.tcp().await;
    let seen = until_closed(&mut extra, Duration::from_secs(5)).await.expect("the 65th was closed");
    assert!(seen.is_empty(), "no TLS record and no byte at all was sent back to the 65th connection");
    println!("Admission: 64 held (open = {}), the 65th closed with {} bytes", server.admission.open_connections(), seen.len());
    assert_eq!(server.admission.open_connections(), 64, "the 65th never held a slot");

    // After one of the 64 closes, a new connection is served.
    drop(held.pop());
    assert!(eventually(Duration::from_secs(5), || server.admission.open_connections() == 63).await);
    let reply = server.get("/login.css", None).await;
    assert_eq!(reply.status, 200);
    println!("Admission: after one closed, a new connection got {}", reply.status);
}

#[tokio::test]
async fn admission_the_total_cap_is_enforced() {
    let server = start_with(Opts { caps: Some((3, 64)), ..Opts::default() }).await;
    let held = [server.tcp().await, server.tcp().await, server.tcp().await];
    assert!(eventually(Duration::from_secs(5), || server.admission.open_connections() == 3).await);
    let mut extra = server.tcp().await;
    let seen = until_closed(&mut extra, Duration::from_secs(5)).await.expect("the 4th was closed");
    assert!(seen.is_empty());
    drop(held);
}

#[tokio::test]
async fn admission_every_permit_comes_back_when_the_connections_close() {
    let server = start().await;
    let mut conns = Vec::new();
    for _ in 0..10 {
        conns.push(server.h1().await);
    }
    for conn in &mut conns {
        conn.send(&Request::get("/login.css").bytes()).await;
        assert_eq!(conn.reply(false).await.expect("an answer").status, 200);
    }
    assert_eq!(server.admission.open_connections(), 10);
    drop(conns);
    assert!(eventually(Duration::from_secs(5), || server.admission.open_connections() == 0 && server.admission.tracked_addresses() == 0).await);
    println!("Admission: ten held, ten given back (open = {}, tracked = {})", server.admission.open_connections(), server.admission.tracked_addresses());
}

// ---- HTTP/2 ----------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn h2_a_request_works_and_carries_the_same_headers_as_http_11() {
    use http_body_util::BodyExt;
    let server = start().await;
    let mut sender = edge_tls::h2_client(&server).await;
    let request = axum::http::Request::builder()
        .uri("https://hw.example.test/login.js")
        .body(http_body_util::Empty::<axum::body::Bytes>::new())
        .expect("a request");
    let response = sender.send_request(request).await.expect("an answer over h2");
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.expect("the body").to_bytes();
    assert_eq!(parts.status, 200);
    assert_eq!(bytes.as_ref(), dist("login.js").as_slice());
    let h1 = server.get("/login.js", None).await;
    for name in [
        "content-type",
        "cache-control",
        "etag",
        "content-length",
        "content-security-policy",
        "strict-transport-security",
        "x-content-type-options",
        "referrer-policy",
        "cross-origin-opener-policy",
        "cross-origin-resource-policy",
        "permissions-policy",
    ] {
        let over_h2 = parts.headers.get(name).map(|v| v.to_str().expect("text").to_owned());
        assert_eq!(over_h2.as_deref(), h1.header(name), "{name} is the same over h2 and http/1.1");
    }
    println!("H2: GET /login.js over h2 gave {} with {} header names", parts.status, parts.headers.len());
}

#[tokio::test]
async fn h2_the_server_announces_its_stream_and_header_list_limits() {
    let server = start().await;
    let mut raw = RawH2::connect(&server).await;
    let settings = raw.handshake().await;
    println!("H2: server settings = {settings:?}");
    assert!(settings.contains(&(0x3, 128)), "at most 128 concurrent streams: {settings:?}");
    assert!(settings.contains(&(0x6, 64 * 1024)), "a header list of at most 64 KiB: {settings:?}");
}

#[tokio::test]
async fn h2_the_129th_concurrent_stream_is_refused_by_the_protocol() {
    let (server, hits) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    for n in 0..129u32 {
        raw.get(1 + 2 * n, "/t/slow?ms=2500").await;
    }
    let refused_stream = 1 + 2 * 128;
    let mut refused = None;
    let mut other_resets = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match raw.read_within(Duration::from_millis(300)).await {
            Read::Frame(frame) if frame.kind == edge_tls::RST_STREAM => {
                if frame.stream == refused_stream {
                    refused = frame.error_code();
                    break;
                }
                other_resets.push(frame.stream);
            }
            Read::Frame(_) => {}
            Read::Quiet if hits.load(Ordering::SeqCst) >= 128 => break,
            Read::Quiet => {}
            Read::Ended => break,
        }
    }
    println!("H2: stream cap: the 129th stream was reset with code {refused:?}, handlers reached = {}", hits.load(Ordering::SeqCst));
    assert_eq!(refused, Some(0x7), "REFUSED_STREAM for the 129th stream");
    assert!(other_resets.is_empty(), "the first 128 streams were not reset: {other_resets:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 128, "exactly 128 requests reached a handler");
}

#[tokio::test]
async fn h2_a_header_list_over_64_kib_is_refused() {
    let (server, hits) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    let block = edge_tls::request_block("/t/hit", &[("x-big", &"a".repeat(70_000))]);
    let mut chunks = block.chunks(16_000);
    raw.send(edge_tls::HEADERS, edge_tls::END_STREAM, 1, chunks.next().expect("a first chunk")).await;
    let rest: Vec<&[u8]> = chunks.collect();
    for (n, chunk) in rest.iter().enumerate() {
        let flags = if n + 1 == rest.len() { edge_tls::END_HEADERS } else { 0 };
        if !raw.try_write(&edge_tls::frame_bytes(edge_tls::CONTINUATION, flags, 1, chunk)).await {
            break;
        }
    }
    let mut answer = None;
    for _ in 0..20 {
        match raw.read_within(Duration::from_millis(500)).await {
            Read::Frame(frame) if frame.stream == 1 || frame.kind == edge_tls::GOAWAY => {
                answer = Some(format!("frame type {} on stream {}", frame.kind, frame.stream));
                break;
            }
            Read::Frame(_) | Read::Quiet => {}
            Read::Ended => {
                answer = Some("the connection was closed".to_owned());
                break;
            }
        }
    }
    println!("H2: a 70,000 byte header was answered with: {answer:?}");
    assert!(answer.is_some(), "the oversized request got an answer or the connection closed");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the request never reached a handler");
}

#[tokio::test]
async fn h2_a_flood_of_stream_resets_ends_in_a_disconnect_and_the_server_stays_up() {
    let (server, hits) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    let mut sent = 0u32;
    let mut cut = false;
    'flood: for n in 0..5000u32 {
        let id = 1 + 2 * n;
        if !raw.try_write(&edge_tls::frame_bytes(edge_tls::HEADERS, edge_tls::END_HEADERS | edge_tls::END_STREAM, id, &edge_tls::request_block("/t/slow?ms=5000", &[]))).await
            || !raw.try_write(&edge_tls::frame_bytes(edge_tls::RST_STREAM, 0, id, &8u32.to_be_bytes())).await
        {
            cut = true;
            break;
        }
        sent += 1;
        if n % 10 == 0 {
            loop {
                match raw.read_within(Duration::from_millis(1)).await {
                    Read::Frame(frame) if frame.kind == edge_tls::GOAWAY => {
                        println!("H2: GOAWAY with code {:?} after {sent} resets", frame.error_code());
                        cut = true;
                        break 'flood;
                    }
                    Read::Frame(_) => {}
                    Read::Quiet => break,
                    Read::Ended => {
                        cut = true;
                        break 'flood;
                    }
                }
            }
        }
    }
    println!("H2: {sent} streams opened and reset before the server cut the connection (cut = {cut}), handlers reached = {}", hits.load(Ordering::SeqCst));
    assert!(cut, "5,000 opened and reset streams did not end in a disconnect");
    assert!(sent <= 1500, "the server tolerated {sent} resets");
    assert!(raw.ends_within(Duration::from_secs(5)).await, "the connection is closed");
    let reply = server.get("/login.css", None).await;
    assert_eq!(reply.status, 200, "the server answers a normal request afterwards");
}

#[tokio::test]
async fn h2_a_flood_of_ping_frames_ends_in_a_disconnect() {
    let (server, _) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    let ping = edge_tls::frame_bytes(edge_tls::PING, 0, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);
    let batch = ping.repeat(1000);
    let mut sent = 0;
    let began = Instant::now();
    while sent < 200_000 {
        if !raw.try_write(&batch).await {
            break;
        }
        sent += 1000;
    }
    let ended = raw.ends_within(Duration::from_secs(8)).await;
    println!("H2: {sent} PING frames sent without reading; connection ended = {ended} after {:?}", began.elapsed());
    assert!(ended, "the connection did not end");
    let reply = server.get("/login.css", None).await;
    assert_eq!(reply.status, 200, "the server answers a normal request afterwards");
}

#[tokio::test]
async fn h2_a_flood_of_settings_frames_ends_in_a_disconnect() {
    let (server, _) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    let settings = edge_tls::frame_bytes(edge_tls::SETTINGS, 0, 0, &[]);
    let batch = settings.repeat(1000);
    let mut sent = 0;
    let began = Instant::now();
    while sent < 200_000 {
        if !raw.try_write(&batch).await {
            break;
        }
        sent += 1000;
    }
    let ended = raw.ends_within(Duration::from_secs(8)).await;
    println!("H2: {sent} SETTINGS frames sent without reading; connection ended = {ended} after {:?}", began.elapsed());
    assert!(ended, "the connection did not end");
    assert_eq!(server.get("/login.css", None).await.status, 200);
}

#[tokio::test]
async fn h2_a_peer_that_does_not_answer_the_keep_alive_ping_is_cut() {
    let (server, _) = quick_server(Timing { header: Duration::from_secs(30), idle: Duration::from_secs(30), ..quick() }).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    raw.get(1, "/t/slow?ms=6000").await;
    let began = Instant::now();
    // The raw client reads and discards every frame and never answers a ping.
    assert!(raw.ends_within(Duration::from_secs(5)).await, "the connection was cut");
    let took = began.elapsed();
    println!("H2: unanswered keep-alive ping: connection cut after {took:?} (interval {:?}, timeout {:?})", quick().h2_ping_interval, quick().h2_ping_timeout);
    assert!(took < Duration::from_secs(4));
}

#[tokio::test]
async fn h2_a_connection_without_requests_is_closed_after_the_idle_time() {
    let (server, _) = quick_server(quick()).await;
    let mut raw = RawH2::connect(&server).await;
    raw.handshake().await;
    let began = Instant::now();
    assert!(raw.ends_within(Duration::from_secs(5)).await, "the idle h2 connection was closed");
    let took = began.elapsed();
    println!("H2: connection with no request closed after {took:?}");
    assert!(took >= quick().header.mul_f32(0.8) && took < quick().header + Duration::from_millis(1500));
}

// ---- HTTP/1.1 limits -------------------------------------------------------------------------------------------------

/// A request with `count` headers in all, the `Host` and the `Connection` among them.
fn with_headers(count: usize) -> Vec<u8> {
    let mut request = Request::get("/login.css").close();
    for n in 0..count - 2 {
        request = request.header(&format!("X-H{n}"), "v");
    }
    request.bytes()
}

#[tokio::test]
async fn h1_100_headers_are_accepted_and_101_are_refused() {
    let server = start().await;
    let mut conn = server.h1().await;
    conn.send(&with_headers(100)).await;
    assert_eq!(conn.reply(false).await.expect("an answer").status, 200, "100 headers");

    let mut conn = server.h1().await;
    conn.send(&with_headers(101)).await;
    let reply = conn.reply(false).await;
    println!("H1: 101 headers -> {:?}", reply.as_ref().map(|r| r.status));
    assert!(reply.is_none_or(|r| r.status == 431), "101 headers are refused");
}

#[tokio::test]
async fn h1_a_request_line_over_64_kib_is_refused() {
    let server = start().await;
    let mut conn = server.h1().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.stream.write_all(&Request::get(&format!("/{}", "a".repeat(70 * 1024))).close().bytes())).await;
    let reply = conn.reply(false).await;
    println!("H1: a request line of 70 KiB -> {:?}", reply.as_ref().map(|r| r.status));
    assert!(reply.is_none_or(|r| r.status == 431 || r.status == 414 || r.status == 400), "it is refused, not served");
}

#[tokio::test]
async fn h1_a_header_block_over_64_kib_is_refused() {
    let server = start().await;
    let mut conn = server.h1().await;
    let mut request = Request::get("/login.css").close();
    for n in 0..8 {
        request = request.header(&format!("X-Big{n}"), &"b".repeat(9000));
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.stream.write_all(&request.bytes())).await;
    let reply = conn.reply(false).await;
    println!("H1: 72 KB of headers -> {:?}", reply.as_ref().map(|r| r.status));
    assert!(reply.is_none_or(|r| r.status == 431 || r.status == 400));
}

#[tokio::test]
async fn h1_a_body_above_the_cap_is_refused_with_the_application_layers_400() {
    let (server, _) = quick_server(Timing::DEFAULT).await;
    let mut request = Request::new("POST", "/t/body").header("Origin", "https://hw.example.test").header("Content-Type", "application/json").close();
    request.body = vec![b'x'; 70_000];
    let reply = server.send(request).await;
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json(), serde_json::json!({ "code": "bad_request" }));
    let mut small = Request::new("POST", "/t/body").header("Origin", "https://hw.example.test").header("Content-Type", "application/json").close();
    small.body = vec![b'x'; 65_536];
    let reply = server.send(small).await;
    assert_eq!((reply.status, reply.text()), (200, "65536".to_owned()), "the cap itself is allowed");
}

#[tokio::test]
async fn h1_a_pipelined_burst_of_100_requests_is_answered_in_order() {
    let server = start().await;
    let mut conn = server.h1().await;
    let mut burst = Vec::new();
    for n in 0..100 {
        let path = if n % 2 == 0 { "/login.js" } else { "/login.css" };
        let request = Request::get(path);
        burst.extend_from_slice(&if n == 99 { request.close() } else { request }.bytes());
    }
    conn.send(&burst).await;
    for n in 0..100 {
        let reply = conn.reply(false).await.unwrap_or_else(|| panic!("answer {n}"));
        let (path, kind) = if n % 2 == 0 { ("login.js", "text/javascript; charset=utf-8") } else { ("login.css", "text/css; charset=utf-8") };
        assert_eq!(reply.status, 200, "answer {n}");
        assert_eq!(reply.header("content-type"), Some(kind), "answer {n} is for request {n}");
        assert_eq!(reply.body, dist(path), "answer {n}");
    }
    println!("H1: 100 pipelined requests answered in order");
}

// ---- proxy mode ------------------------------------------------------------------------------------------------------

async fn proxy_server() -> (Started, Hits) {
    let hits: Hits = Arc::default();
    let server = start_with(Opts { extra: routes(&hits), trusted_proxies: Some("127.0.0.1/32"), ..Opts::default() }).await;
    (server, hits)
}

async fn plain(server: &Started, request: Request) -> Option<Reply> {
    let mut conn = edge_tls::Conn::new(server.tcp().await);
    conn.send(&request.close().bytes()).await;
    conn.reply(false).await
}

#[tokio::test]
async fn proxy_plain_http_from_a_trusted_peer_is_served() {
    let (server, _) = proxy_server().await;
    let reply = plain(&server, Request::get("/api/session").header("X-Forwarded-Proto", "https")).await.expect("an answer");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["state"], "anonymous");
    let page = plain(&server, Request::get("/").header("X-Forwarded-Proto", "https")).await.expect("an answer");
    assert_eq!((page.status, page.body), (200, dist("login.html")));
    println!("Proxy: plain HTTP from the trusted peer 127.0.0.1 -> {}", reply.status);
}

#[tokio::test]
async fn proxy_the_forwarded_address_is_the_client_address() {
    let (server, _) = proxy_server().await;
    let reply = plain(&server, Request::get("/t/echo").header("X-Forwarded-Proto", "https").header("X-Forwarded-For", "203.0.113.9"))
        .await
        .expect("an answer");
    assert_eq!(reply.json(), serde_json::json!({ "client": "203.0.113.9", "transport": "Proxy" }));
    let direct = plain(&server, Request::get("/t/echo").header("X-Forwarded-Proto", "https")).await.expect("an answer");
    assert_eq!(direct.json()["client"], "127.0.0.1", "without a forwarded address the proxy itself is the client");
    println!("Proxy: X-Forwarded-For 203.0.113.9 -> client {}", reply.json()["client"]);
}

#[tokio::test]
async fn proxy_a_forwarded_scheme_of_http_or_none_is_400() {
    let (server, _) = proxy_server().await;
    for request in [
        Request::get("/api/session").header("X-Forwarded-Proto", "http"),
        Request::get("/api/session"),
        Request::get("/api/session").header("X-Forwarded-Proto", "https").header("X-Forwarded-Proto", "https"),
    ] {
        let reply = plain(&server, request.clone()).await.expect("an answer");
        assert_eq!(reply.status, 400, "{:?}", request.headers);
    }
    println!("Proxy: X-Forwarded-Proto http -> 400, none -> 400");
}

#[tokio::test]
async fn proxy_a_peer_that_is_not_a_trusted_proxy_is_refused_before_a_byte_is_read() {
    let hits: Hits = Arc::default();
    let server = start_with(Opts { extra: routes(&hits), trusted_proxies: Some("192.0.2.0/24"), ..Opts::default() }).await;
    let mut tcp = server.tcp().await;
    let _ = tcp.write_all(&Request::get("/t/echo").header("X-Forwarded-Proto", "https").close().bytes()).await;
    let seen = until_closed(&mut tcp, Duration::from_secs(5)).await.expect("the connection was closed");
    assert!(seen.is_empty(), "an untrusted peer gets no answer");
    assert_eq!(server.admission.open_connections(), 0);
    println!("Proxy: a peer outside the trusted list was closed with {} bytes", seen.len());
}

#[tokio::test]
async fn proxy_in_files_mode_forwarded_headers_are_ignored() {
    let hits: Hits = Arc::default();
    let server = start_with(Opts { extra: routes(&hits), ..Opts::default() }).await;
    let reply = server.send(Request::get("/t/echo").header("X-Forwarded-For", "203.0.113.9").header("X-Forwarded-Proto", "http").close()).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), serde_json::json!({ "client": "127.0.0.1", "transport": "Tls" }));
    println!("Proxy: files mode ignored X-Forwarded-For 203.0.113.9 -> client {}", reply.json()["client"]);
}

#[tokio::test]
async fn proxy_in_files_mode_a_plain_http_client_never_gets_a_response() {
    let (server, hits) = quick_server(quick()).await;
    let mut tcp = server.tcp().await;
    tcp.write_all(&Request::get("/t/hit").close().bytes()).await.expect("write");
    let seen = until_closed(&mut tcp, Duration::from_secs(5)).await.expect("the connection was closed");
    assert!(!seen.starts_with(b"HTTP/"), "no HTTP over a connection that has not completed TLS");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn proxy_an_edge_needs_a_tls_acceptor_exactly_when_the_mode_serves_tls() {
    use holzwave_server::http::app::{Edge, EdgeError};
    use holzwave_server::proxy::{Admission, Mode, TrustedProxies};
    let admission = Admission::with_defaults(Mode::Files, TrustedProxies::none()).expect("admission");
    assert_eq!(Edge::new(Router::new(), None, admission, Mode::Files).err(), Some(EdgeError::NeedsTls));
    let cidr = holzwave_server::config::Cidr::parse("127.0.0.1/32").expect("a network");
    let proxies = TrustedProxies::new(&[cidr]).expect("proxies");
    let admission = Admission::with_defaults(Mode::Proxy, proxies).expect("admission");
    assert!(Edge::new(Router::new(), None, admission, Mode::Proxy).is_ok());
}

// ---- panic isolation -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn panic_a_handler_that_panics_closes_that_connection_and_the_next_one_works() {
    let (server, _) = quick_server(Timing::DEFAULT).await;
    for _ in 0..20 {
        let mut conn = server.h1().await;
        conn.send(&Request::get("/t/panic").bytes()).await;
        assert!(conn.reply(false).await.is_none(), "a panicking handler answers nothing and closes the connection");
        assert_eq!(server.get("/login.css", None).await.status, 200, "the next connection works");
    }
    assert!(eventually(Duration::from_secs(5), || server.admission.open_connections() == 0).await, "no connection task is left behind");
    println!("Panic: 20 panics, 20 later requests answered, open connections = {}", server.admission.open_connections());
}

#[tokio::test]
async fn panic_over_http_2_ends_that_stream_and_the_server_goes_on() {
    use http_body_util::BodyExt;
    let (server, _) = quick_server(Timing::DEFAULT).await;
    let mut sender = edge_tls::h2_client(&server).await;
    let request = |path: &str| {
        axum::http::Request::builder().uri(format!("https://hw.example.test{path}")).body(http_body_util::Empty::<axum::body::Bytes>::new()).expect("a request")
    };
    let first = sender.send_request(request("/t/panic")).await;
    let failed = match first {
        Err(_) => true,
        Ok(response) => response.into_body().collect().await.is_err() || false,
    };
    assert!(failed, "the stream of a panicking handler ends without an answer");
    assert_eq!(server.get("/login.css", None).await.status, 200, "another connection is served");
    println!("Panic: over h2 the stream failed and a new connection was served");
}

// ---- shutdown --------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_stops_accepting_at_once() {
    let (mut server, _) = quick_server(quick()).await;
    server.shutdown.trigger();
    let task = server.task.take().expect("the loop");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the loop ended").expect("no panic");
    let refused = eventually(Duration::from_secs(2), || std::net::TcpStream::connect_timeout(&server.addr, Duration::from_millis(200)).is_err()).await;
    assert!(refused, "a new connection is refused after the shutdown");
    println!("Shutdown: after the signal a connect to the port fails: {refused}");
}

#[tokio::test]
async fn shutdown_lets_a_request_in_flight_finish_inside_the_grace_time() {
    let (server, hits) = quick_server(quick()).await;
    let mut conn = server.h1().await;
    conn.send(&Request::get("/t/slow?ms=600").bytes()).await;
    assert!(eventually(Duration::from_secs(3), || hits.load(Ordering::SeqCst) == 1).await, "the request is in flight");
    let began = Instant::now();
    server.shutdown.trigger();
    let reply = conn.reply(false).await.expect("the request in flight was answered");
    assert_eq!((reply.status, reply.text()), (200, "slow".to_owned()));
    let rest = until_closed(&mut conn.stream, Duration::from_secs(3)).await;
    println!("Shutdown: in-flight request answered {} after {:?}, then the connection closed: {}", reply.status, began.elapsed(), rest.is_some());
    assert!(rest.is_some(), "the connection is closed after the answer");
    assert!(eventually(Duration::from_secs(3), || server.admission.open_connections() == 0).await);
}

#[tokio::test]
async fn shutdown_drops_a_request_that_outlasts_the_grace_time() {
    let grace = Duration::from_millis(500);
    let (mut server, hits) = quick_server(Timing { grace, ..quick() }).await;
    let mut conn = server.h1().await;
    conn.send(&Request::get("/t/slow?ms=8000").bytes()).await;
    assert!(eventually(Duration::from_secs(3), || hits.load(Ordering::SeqCst) == 1).await);
    let began = Instant::now();
    server.shutdown.trigger();
    let seen = until_closed(&mut conn.stream, Duration::from_secs(5)).await.expect("the connection was dropped");
    let took = began.elapsed();
    println!("Shutdown: a request that outlasts the grace time was dropped after {took:?} (grace {grace:?})");
    assert!(!seen.starts_with(b"HTTP/"), "no answer to a dropped request");
    assert!(took >= grace.mul_f32(0.8) && took < grace + Duration::from_millis(1500), "{took:?}");
    let task = server.task.take().expect("the loop");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the loop ended").expect("no panic");
    assert_eq!(server.admission.open_connections(), 0, "all permits are released");
}

#[tokio::test]
async fn shutdown_the_signal_is_a_request_extension_and_becomes_ready_on_trigger() {
    let (server, _) = quick_server(quick()).await;
    let before = server.send(Request::get("/t/signalled").close()).await;
    assert_eq!(before.text(), "no");
    let mut conn = server.h1().await;
    conn.send(&Request::get("/t/signal").bytes()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    server.shutdown.trigger();
    let reply = conn.reply(false).await.expect("the handler saw the signal and answered");
    assert_eq!((reply.status, reply.text()), (200, "closing".to_owned()));
    println!("Shutdown: the handler's copy of the signal became ready on trigger");
}

#[tokio::test]
async fn shutdown_a_second_trigger_changes_nothing() {
    let (mut server, _) = quick_server(quick()).await;
    server.shutdown.trigger();
    server.shutdown.trigger();
    assert!(server.shutdown.is_triggered());
    let task = server.task.take().expect("the loop");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the loop ended").expect("no panic");
    server.shutdown.trigger();
    assert!(server.shutdown.is_triggered());
    let copy = server.shutdown.clone();
    tokio::time::timeout(Duration::from_secs(1), copy.wait()).await.expect("a copy is ready at once");
}

#[tokio::test]
async fn shutdown_an_idle_keep_alive_connection_is_closed_at_once_and_every_permit_comes_back() {
    let (mut server, _) = quick_server(Timing { grace: Duration::from_secs(5), idle: Duration::from_secs(30), ..quick() }).await;
    let mut conns = Vec::new();
    for _ in 0..3 {
        let mut conn = server.h1().await;
        conn.send(&Request::get("/login.css").bytes()).await;
        assert_eq!(conn.reply(false).await.expect("an answer").status, 200);
        conns.push(conn);
    }
    let began = Instant::now();
    server.shutdown.trigger();
    for conn in &mut conns {
        until_closed(&mut conn.stream, Duration::from_secs(3)).await.expect("the idle connection was closed");
    }
    let task = server.task.take().expect("the loop");
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("the loop ended").expect("no panic");
    println!("Shutdown: three idle connections closed within {:?}, open = {}", began.elapsed(), server.admission.open_connections());
    assert!(began.elapsed() < Duration::from_secs(2), "they did not wait for the grace time");
    assert_eq!(server.admission.open_connections(), 0);
}

#[test]
fn shutdown_the_signal_is_shared_by_its_copies() {
    let signal = Shutdown::new();
    let copy = signal.clone();
    assert!(!signal.is_triggered() && !copy.is_triggered());
    copy.trigger();
    copy.trigger();
    assert!(signal.is_triggered() && copy.is_triggered());
}

// ---- the health socket -----------------------------------------------------------------------------------------------

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use tokio::net::{UnixListener, UnixStream};

async fn health_request(dir: &std::path::Path, request: &str) -> Option<Reply> {
    let stream = UnixStream::connect(dir.join(health::SOCKET_NAME)).await.ok()?;
    let mut conn = edge_tls::Conn::new(stream);
    conn.send(request.as_bytes()).await;
    conn.reply(false).await
}

const HEALTHZ: &str = "GET /healthz HTTP/1.1\r\nHost: health\r\nConnection: close\r\n\r\n";

#[tokio::test]
async fn health_the_socket_is_a_private_file_in_the_runtime_directory_and_nothing_else_is_left() {
    let dir = ShortDir::new();
    let shutdown = Shutdown::new();
    let server = health::serve(dir.path(), Arc::new(Readiness::new()), shutdown.clone()).await.expect("serve");
    let path = dir.path().join(health::SOCKET_NAME);
    assert_eq!(server.path(), path);
    let meta = std::fs::symlink_metadata(&path).expect("the socket exists");
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600, "mode 0600");
    let names: Vec<String> = std::fs::read_dir(dir.path()).expect("list").map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, [health::SOCKET_NAME], "no temporary file is left behind");
    println!("Health: {} is a socket with mode {:o}", path.display(), meta.permissions().mode() & 0o777);
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn health_200_ok_when_every_check_passes_and_503_not_ready_when_one_does_not() {
    let dir = ShortDir::new();
    let shutdown = Shutdown::new();
    let readiness = Arc::new(Readiness::new());
    let store = Arc::new(AtomicBool::new(true));
    readiness.register("always", || true);
    let flag = store.clone();
    readiness.register("store", move || flag.load(Ordering::SeqCst));
    let server = health::serve(dir.path(), readiness.clone(), shutdown.clone()).await.expect("serve");

    let ok = health_request(dir.path(), HEALTHZ).await.expect("an answer");
    assert_eq!((ok.status, ok.text()), (200, "ok".to_owned()));
    store.store(false, Ordering::SeqCst);
    let not_ready = health_request(dir.path(), HEALTHZ).await.expect("an answer");
    assert_eq!((not_ready.status, not_ready.text()), (503, "not ready".to_owned()));
    assert_eq!(readiness.failing(), ["store"]);
    store.store(true, Ordering::SeqCst);
    assert_eq!(health_request(dir.path(), HEALTHZ).await.expect("an answer").status, 200);
    println!("Health: checks passing -> 200 ok, one failing -> 503 not ready");
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn health_every_other_path_and_method_is_404_with_no_detail() {
    let dir = ShortDir::new();
    let shutdown = Shutdown::new();
    let server = health::serve(dir.path(), Arc::new(Readiness::new()), shutdown.clone()).await.expect("serve");
    for request in [
        "GET / HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        "GET /health HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        "GET /healthz/ HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        "GET /metrics HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        "GET /api/session HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
        "POST /healthz HTTP/1.1\r\nHost: h\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "DELETE /healthz HTTP/1.1\r\nHost: h\r\nConnection: close\r\n\r\n",
    ] {
        let reply = health_request(dir.path(), request).await.expect("an answer");
        assert_eq!(reply.status, 404, "{request:?}");
        assert!(reply.body.is_empty(), "{request:?}: no detail");
    }
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn health_a_ninth_simultaneous_connection_is_closed() {
    let dir = ShortDir::new();
    let shutdown = Shutdown::new();
    let server = health::serve(dir.path(), Arc::new(Readiness::new()), shutdown.clone()).await.expect("serve");
    let path = dir.path().join(health::SOCKET_NAME);
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(UnixStream::connect(&path).await.expect("connect"));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut ninth = UnixStream::connect(&path).await.expect("connect");
    let seen = until_closed(&mut ninth, Duration::from_secs(2)).await.expect("the ninth connection was closed");
    assert!(seen.is_empty());
    println!("Health: 8 connections held, the 9th was closed with {} bytes", seen.len());
    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(health_request(dir.path(), HEALTHZ).await.expect("an answer").status, 200, "a slot is free again");
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn health_a_stale_socket_file_from_an_earlier_run_is_replaced() {
    let dir = ShortDir::new();
    let path = dir.path().join(health::SOCKET_NAME);
    drop(std::os::unix::net::UnixListener::bind(&path).expect("a socket that nobody listens on"));
    assert!(std::fs::symlink_metadata(&path).expect("it is left behind").file_type().is_socket());
    let shutdown = Shutdown::new();
    let server = health::serve(dir.path(), Arc::new(Readiness::new()), shutdown.clone()).await.expect("the stale file is replaced");
    assert_eq!(health_request(dir.path(), HEALTHZ).await.expect("an answer").status, 200);
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn health_a_regular_file_at_the_path_is_refused_with_a_sentence_and_left_alone() {
    let dir = ShortDir::new();
    let path = dir.path().join(health::SOCKET_NAME);
    std::fs::write(&path, b"precious").expect("a file");
    let error = health::serve(dir.path(), Arc::new(Readiness::new()), Shutdown::new()).await.err().expect("a refusal");
    println!("Health: refusal for a regular file: {error}");
    assert!(error.to_string().contains("not a socket") && error.to_string().contains(&path.display().to_string()), "{error}");
    assert_eq!(std::fs::read(&path).expect("still there"), b"precious");
    // A directory and a link are refused the same way.
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir(&path).expect("a directory");
    assert!(health::serve(dir.path(), Arc::new(Readiness::new()), Shutdown::new()).await.is_err());
    std::fs::remove_dir(&path).expect("remove");
    std::os::unix::fs::symlink("/etc/hosts", &path).expect("a link");
    assert!(health::serve(dir.path(), Arc::new(Readiness::new()), Shutdown::new()).await.is_err());
    assert!(std::fs::symlink_metadata(&path).expect("the link is still there").file_type().is_symlink());
}

#[tokio::test]
async fn health_a_socket_that_another_server_listens_on_is_not_replaced() {
    let dir = ShortDir::new();
    let path = dir.path().join(health::SOCKET_NAME);
    let _other = UnixListener::bind(&path).expect("another server's socket");
    let error = health::serve(dir.path(), Arc::new(Readiness::new()), Shutdown::new()).await.err().expect("a refusal");
    println!("Health: refusal for a live socket: {error}");
    assert!(UnixStream::connect(&path).await.is_ok(), "the other socket still accepts");
}

#[tokio::test]
async fn health_the_runtime_directory_is_checked_before_the_socket_is_made() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = ShortDir::new();
    let missing = dir.path().join("missing");
    let error = health::serve(&missing, Arc::new(Readiness::new()), Shutdown::new()).await.err().expect("a refusal");
    assert!(error.to_string().contains(&missing.display().to_string()), "{error}");

    let open = dir.path().join("open");
    std::fs::create_dir(&open).expect("dir");
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let error = health::serve(&open, Arc::new(Readiness::new()), Shutdown::new()).await.err().expect("a refusal");
    println!("Health: refusal for a world-writable directory: {error}");
    assert!(error.to_string().contains("writable"), "{error}");
    assert!(!open.join(health::SOCKET_NAME).exists(), "no socket was made in it");

    let file = dir.path().join("file");
    std::fs::write(&file, b"x").expect("file");
    assert!(health::serve(&file, Arc::new(Readiness::new()), Shutdown::new()).await.is_err());
}

#[tokio::test]
async fn health_the_socket_file_is_removed_on_shutdown() {
    let dir = ShortDir::new();
    let shutdown = Shutdown::new();
    let server = health::serve(dir.path(), Arc::new(Readiness::new()), shutdown.clone()).await.expect("serve");
    let path = dir.path().join(health::SOCKET_NAME);
    assert!(path.exists());
    shutdown.trigger();
    tokio::time::timeout(Duration::from_secs(3), server.finished()).await.expect("the health server ended");
    assert!(!path.exists(), "the socket file is gone");
    println!("Health: the socket file was removed on shutdown");
}

#[test]
fn health_there_is_no_network_port_in_the_health_code() {
    let source = include_str!("../src/http/health.rs");
    for word in ["TcpListener", "TcpStream", "0.0.0.0", "127.0.0.1", "SocketAddr"] {
        assert!(!source.contains(word), "health.rs has no network port ({word})");
    }
}

// ---- the healthcheck command -----------------------------------------------------------------------------------------

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    took: Duration,
}

/// Runs `holzwave-server healthcheck` with a configuration file that names `runtime_dir` (and some other keys it ignores).
async fn healthcheck(runtime_dir: &std::path::Path, config: &std::path::Path) -> Ran {
    let began = Instant::now();
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
            .arg("healthcheck")
            .env_clear()
            .env("HOLZWAVE_SERVER_CONFIG", config)
            .output(),
    )
    .await
    .expect("the command ended")
    .expect("the command ran");
    let _ = runtime_dir;
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        took: began.elapsed(),
    }
}

fn config_file(dir: &ShortDir, runtime_dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.path().join("server.json");
    let text = serde_json::json!({
        "runtime_dir": runtime_dir,
        "public_url": "https://hw.example.test",
        "listen": "0.0.0.0:8443",
        "tls": { "mode": "files" },
        "unknown_future_key": [1, 2, 3],
    });
    std::fs::write(&path, text.to_string()).expect("write the configuration");
    path
}

#[tokio::test]
async fn healthcheck_prints_healthy_and_exits_0_when_the_server_is_ready() {
    let dir = ShortDir::new();
    let runtime = dir.path().join("run");
    std::fs::create_dir(&runtime).expect("dir");
    let shutdown = Shutdown::new();
    let server = health::serve(&runtime, Arc::new(Readiness::new()), shutdown.clone()).await.expect("serve");
    let ran = healthcheck(&runtime, &config_file(&dir, &runtime)).await;
    println!("Health: healthcheck on a ready server: exit {:?}, stdout {:?}, {:?}", ran.code, ran.stdout, ran.took);
    assert_eq!((ran.code, ran.stdout.as_str(), ran.stderr.as_str()), (Some(0), "healthy\n", ""));
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn healthcheck_exits_1_with_one_sentence_when_a_check_fails() {
    let dir = ShortDir::new();
    let runtime = dir.path().join("run");
    std::fs::create_dir(&runtime).expect("dir");
    let shutdown = Shutdown::new();
    let readiness = Arc::new(Readiness::new());
    readiness.register("store", || false);
    let server = health::serve(&runtime, readiness, shutdown.clone()).await.expect("serve");
    let ran = healthcheck(&runtime, &config_file(&dir, &runtime)).await;
    println!("Health: healthcheck on a server that is not ready: exit {:?}, stderr {:?}", ran.code, ran.stderr);
    assert_eq!(ran.code, Some(1));
    assert_eq!(ran.stdout, "");
    assert_eq!(ran.stderr.lines().count(), 1, "one sentence: {:?}", ran.stderr);
    assert!(ran.stderr.contains("not ready"));
    shutdown.trigger();
    server.finished().await;
}

#[tokio::test]
async fn healthcheck_exits_1_with_one_sentence_when_there_is_no_socket() {
    let dir = ShortDir::new();
    let runtime = dir.path().join("run");
    std::fs::create_dir(&runtime).expect("dir");
    let ran = healthcheck(&runtime, &config_file(&dir, &runtime)).await;
    println!("Health: healthcheck without a socket: exit {:?}, stderr {:?}", ran.code, ran.stderr);
    assert_eq!(ran.code, Some(1));
    assert_eq!(ran.stderr.lines().count(), 1, "{:?}", ran.stderr);
    assert!(ran.stderr.contains(&runtime.display().to_string()), "it names the directory: {:?}", ran.stderr);
}

#[tokio::test]
async fn healthcheck_exits_1_on_an_answer_that_is_not_the_servers() {
    let dir = ShortDir::new();
    let runtime = dir.path().join("run");
    std::fs::create_dir(&runtime).expect("dir");
    let listener = UnixListener::bind(runtime.join(health::SOCKET_NAME)).expect("a fake server");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut sink = [0u8; 512];
            let _ = stream.read(&mut sink).await;
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nall is well").await;
        }
    });
    let ran = healthcheck(&runtime, &config_file(&dir, &runtime)).await;
    println!("Health: healthcheck on a garbled answer: exit {:?}, stderr {:?}", ran.code, ran.stderr);
    assert_eq!(ran.code, Some(1));
    assert_eq!(ran.stdout, "");
    assert_eq!(ran.stderr.lines().count(), 1);
}

#[tokio::test]
async fn healthcheck_returns_within_3_seconds_when_the_server_accepts_and_never_answers() {
    let dir = ShortDir::new();
    let runtime = dir.path().join("run");
    std::fs::create_dir(&runtime).expect("dir");
    let listener = UnixListener::bind(runtime.join(health::SOCKET_NAME)).expect("a fake server");
    let _holder = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let ran = healthcheck(&runtime, &config_file(&dir, &runtime)).await;
    println!("Health: healthcheck against a server that never answers: exit {:?} after {:?}", ran.code, ran.took);
    assert_eq!(ran.code, Some(1));
    assert!(ran.took < Duration::from_millis(3600), "{:?}", ran.took);
    assert!(ran.took >= Duration::from_millis(2500), "it waited for the answer: {:?}", ran.took);
    assert_eq!(ran.stderr.lines().count(), 1);
}

#[tokio::test]
async fn healthcheck_reads_only_the_runtime_directory_and_survives_a_missing_or_broken_file() {
    let dir = ShortDir::new();
    // No file at all: the default directory is asked, and nothing listens there.
    let ran = healthcheck(dir.path(), &dir.path().join("none.json")).await;
    assert_eq!(ran.code, Some(1));
    assert!(ran.stderr.contains("/run/holzwave"), "{:?}", ran.stderr);
    // A file that is not JSON is a sentence, not a guess.
    let broken = dir.path().join("broken.json");
    std::fs::write(&broken, b"runtime_dir = nope").expect("write");
    let ran = healthcheck(dir.path(), &broken).await;
    assert_eq!(ran.code, Some(1));
    assert_eq!(ran.stderr.lines().count(), 1);
    assert!(ran.stderr.contains("not valid"), "{:?}", ran.stderr);
    // A file without the key uses the default directory.
    let bare = dir.path().join("bare.json");
    std::fs::write(&bare, br#"{"listen": "0.0.0.0:1"}"#).expect("write");
    let ran = healthcheck(dir.path(), &bare).await;
    assert!(ran.stderr.contains("/run/holzwave"), "{:?}", ran.stderr);
    println!("Health: a missing file, a broken file and a file without the key were each one sentence");
}

#[tokio::test]
async fn healthcheck_takes_no_arguments() {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
        .args(["healthcheck", "--now"])
        .env_clear()
        .output()
        .await
        .expect("the command ran");
    assert_eq!(output.status.code(), Some(2), "a call the server does not understand");
}

#[tokio::test]
async fn health_the_probe_tells_the_four_outcomes_apart() {
    let dir = ShortDir::new();
    let runtime = dir.path().to_path_buf();
    let ask = |runtime: std::path::PathBuf| async move {
        tokio::task::spawn_blocking(move || health::probe(&runtime, health::PROBE_TIMEOUT)).await.expect("the probe ran")
    };
    assert_eq!(ask(runtime.clone()).await, Health::Unreachable, "no socket");
    let shutdown = Shutdown::new();
    let readiness = Arc::new(Readiness::new());
    let server = health::serve(&runtime, readiness.clone(), shutdown.clone()).await.expect("serve");
    assert_eq!(ask(runtime.clone()).await, Health::Healthy);
    readiness.register("later", || false);
    assert_eq!(ask(runtime.clone()).await, Health::NotReady);
    shutdown.trigger();
    server.finished().await;
}
