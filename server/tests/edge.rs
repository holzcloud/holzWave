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
