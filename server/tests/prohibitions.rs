//! The four prohibitions of the specification as tests, from outside: the routes that exist, the headers and the policy of every
//! answer, the page that leaves its origin nowhere, the cross-site matrix, the start of the server, the deployment files and the
//! spawn path, and the source guards.
//!
//! Every guard has a negative control (`negative_control_*`): a pure test that feeds the guard a deliberately wrong input and
//! requires it to report. A green run therefore means that the guards ran and could have caught something.
//!
//! Two assemblies are used. The real binary, started as a process (`real_process.rs`), answers what a client on the network gets
//! with no cookie; the in-process assembly of the unlock suite (`unlock_env.rs`: the same router and the same real workers, with a
//! manual clock) answers what needs sessions of a given age and many throwaway people. The binary is built with the
//! `in-process-workers` feature; the path through the spawn helper belongs to the isolation and image tests.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/parity_env.rs"]
mod parity_env;
#[path = "support/real_process.rs"]
mod real_process;
#[path = "support/route_walk.rs"]
mod route_walk;
#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/sup_env.rs"]
mod sup_env;
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/unlock_env.rs"]
mod unlock_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use edge::{HOST, ORIGIN};
use edge_tls::{Reply, Request};
use real_process::{Client, Site};
use route_walk::{Expect, Kind, Parts, Row, Seen};
use serde_json::{Value, json};
use unlock_env::{PASSWORD, UnlockEnv, User};

// ---- where the sources are -------------------------------------------------------------------------------------------------

fn server_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    server_root().parent().expect("the repository root").to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The router modules: every file of `server/src/http`.
fn router_files() -> Vec<(String, String)> {
    let dir = server_root().join("src/http");
    let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
        .expect("the http directory")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), read(&e.path())))
        .collect();
    files.sort();
    files
}

// ---- a log of what the run saw -----------------------------------------------------------------------------------------------

/// Every `Set-Cookie` line and every answer status any test of this file saw, kept for the summary and the cookie check.
#[derive(Default)]
struct Ledger {
    cookies: std::sync::Mutex<BTreeSet<String>>,
    problems: std::sync::Mutex<Vec<String>>,
}

impl Ledger {
    fn note(&self, reply: &Reply) {
        let mut cookies = self.cookies.lock().expect("lock");
        for line in reply.all("set-cookie") {
            cookies.insert(redact(line));
            self.problems.lock().expect("lock").extend(route_walk::check_set_cookie(line));
        }
    }
}

/// A `Set-Cookie` line with the value cut to its shape: the line is printed in the summary, the value never is.
fn redact(line: &str) -> String {
    match line.split_once(';') {
        Some((first, rest)) => {
            let (name, value) = first.split_once('=').unwrap_or((first, ""));
            format!("{name}=<{} chars>;{rest}", value.len())
        }
        None => line.to_owned(),
    }
}

// =============================================================================================================================
// Pure tests: the guards and their negative controls
// =============================================================================================================================

fn contract_header_set(host: &str) -> Vec<(String, String)> {
    let policy = route_walk::contract_policy(host);
    let csp: Vec<String> = policy.iter().map(|(d, s)| format!("{d} {}", s.iter().cloned().collect::<Vec<_>>().join(" "))).collect();
    let mut headers: Vec<(String, String)> = route_walk::FIXED_HEADERS.iter().map(|(n, v)| ((*n).to_owned(), (*v).to_owned())).collect();
    headers.push(("content-security-policy".to_owned(), csp.join("; ")));
    headers.push(("cross-origin-resource-policy".to_owned(), "same-origin".to_owned()));
    headers.push(("cache-control".to_owned(), "no-store".to_owned()));
    headers
}

fn problems_of(status: u16, path: &str, headers: &[(String, String)]) -> Vec<String> {
    route_walk::check_headers(&Seen { status, path, headers }, HOST)
}

#[test]
fn headers_the_contract_set_passes_and_each_status_is_judged_alike() {
    let headers = contract_header_set(HOST);
    for status in [200, 302, 400, 401, 403, 404, 405, 413, 415, 421] {
        assert!(problems_of(status, "/api/me", &headers).is_empty(), "the contract set passes for {status}");
    }
}

#[test]
fn negative_control_headers_a_missing_header_a_wrong_value_and_a_sharing_header_are_reported() {
    let base = contract_header_set(HOST);
    for (name, _) in route_walk::FIXED_HEADERS.iter().chain(&[("content-security-policy", ""), ("cross-origin-resource-policy", "")]) {
        let mut missing = base.clone();
        missing.retain(|(n, _)| n != name);
        let found = problems_of(404, "/nowhere", &missing);
        assert!(found.iter().any(|p| p.contains(name)), "a response without {name} is reported: {found:?}");
    }
    let mut wrong = base.clone();
    wrong.iter_mut().find(|(n, _)| n == "strict-transport-security").expect("hsts").1 = "max-age=1".to_owned();
    assert!(!problems_of(200, "/", &wrong).is_empty(), "a wrong value is reported");
    let mut shared = base.clone();
    shared.push(("access-control-allow-origin".to_owned(), "*".to_owned()));
    assert!(problems_of(200, "/", &shared).iter().any(|p| p.contains("access-control-allow-origin")), "a sharing header is reported");
    let mut cached = base;
    cached.retain(|(n, _)| n != "cache-control");
    assert!(!problems_of(401, "/api/session", &cached).is_empty(), "an api answer without no-store is reported");
    assert!(problems_of(200, "/login.js", &cached).is_empty(), "a page file needs no no-store");
}

#[test]
fn policy_the_contract_policy_with_the_public_host_passes() {
    let header = contract_header_set(HOST).into_iter().find(|(n, _)| n == "content-security-policy").expect("csp").1;
    assert_eq!(route_walk::check_csp(&header, HOST), Vec::<String>::new());
    // The product's own policy text for the same host equals the contract's, directive by directive.
    let product = holzwave_server::http::headers::content_security_policy(HOST);
    assert_eq!(route_walk::parse_csp(&product), route_walk::contract_policy(HOST));
    // The order of sources and of directives does not matter.
    let shuffled = "frame-ancestors 'none'; connect-src wss://hw.example.test 'self'; default-src 'self'; script-src 'self'; style-src 'self'; \
img-src 'self'; media-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'";
    assert!(route_walk::check_csp(shuffled, HOST).is_empty());
}

#[test]
fn negative_control_policy_every_unsafe_source_a_foreign_host_and_any_difference_is_reported() {
    let good = holzwave_server::http::headers::content_security_policy(HOST);
    let with = |from: &str, to: &str| good.replacen(from, to, 1);
    let cases: Vec<(&str, String)> = vec![
        ("unsafe-inline", with("script-src 'self'", "script-src 'self' 'unsafe-inline'")),
        ("unsafe-eval", with("script-src 'self'", "script-src 'self' 'unsafe-eval'")),
        ("data:", with("img-src 'self'", "img-src 'self' data:")),
        ("blob:", with("media-src 'self'", "media-src 'self' blob:")),
        ("wildcard", with("connect-src 'self'", "connect-src * 'self'")),
        ("a whole scheme", with("font-src 'self'", "font-src 'self' https:")),
        ("a foreign host", with("img-src 'self'", "img-src 'self' https://cdn.example.net")),
        ("a foreign socket host", with("wss://hw.example.test", "wss://evil.example.test")),
        ("a changed directive", with("object-src 'none'", "object-src 'self'")),
        ("a missing directive", with("; frame-ancestors 'none'", "")),
        ("an extra directive", format!("{good}; sandbox")),
        ("a nonce", with("script-src 'self'", "script-src 'self' 'nonce-abc'")),
    ];
    for (what, header) in cases {
        assert!(!route_walk::check_csp(&header, HOST).is_empty(), "{what} is reported");
    }
}

#[test]
fn negative_control_cookies_each_missing_flag_and_each_wrong_value_is_reported() {
    use holzwave_server::http::cookies;
    let value = "A".repeat(43);
    let good = [
        cookies::session(&value).expect("session").to_str().expect("text").to_owned(),
        cookies::delete_session().to_str().expect("text").to_owned(),
        cookies::oidc("abc123").expect("oidc").to_str().expect("text").to_owned(),
        cookies::delete_oidc().to_str().expect("text").to_owned(),
        cookies::pending("abc123").expect("pending").to_str().expect("text").to_owned(),
        cookies::delete_pending().to_str().expect("text").to_owned(),
    ];
    for line in &good {
        assert!(route_walk::check_set_cookie(line).is_empty(), "the writer's own cookie passes: {line}: {:?}", route_walk::check_set_cookie(line));
    }
    let session = format!("__Host-hw_session={value}; Secure; HttpOnly; SameSite=Strict; Path=/");
    let wrong: Vec<(&str, String)> = vec![
        ("no Secure", session.replace("Secure; ", "")),
        ("no HttpOnly", session.replace("HttpOnly; ", "")),
        ("SameSite Lax on the session", session.replace("Strict", "Lax")),
        ("a Domain", format!("{session}; Domain=example.test")),
        ("a path other than /", session.replace("Path=/", "Path=/api")),
        ("a short session value", "__Host-hw_session=abc; Secure; HttpOnly; SameSite=Strict; Path=/".to_owned()),
        ("a session value with a bad character", format!("__Host-hw_session={}+; Secure; HttpOnly; SameSite=Strict; Path=/", "A".repeat(42))),
        ("a session with a Max-Age", format!("{session}; Max-Age=60")),
        ("oidc without a Max-Age", "__Host-hw_oidc=abc; Secure; HttpOnly; SameSite=Lax; Path=/".to_owned()),
        ("oidc with another Max-Age", "__Host-hw_oidc=abc; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=900".to_owned()),
        ("oidc Strict", "__Host-hw_oidc=abc; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=600".to_owned()),
        ("an unknown cookie", "sid=abc; Secure; HttpOnly; SameSite=Strict; Path=/".to_owned()),
    ];
    for (what, line) in wrong {
        assert!(!route_walk::check_set_cookie(&line).is_empty(), "{what} is reported");
    }
}

#[test]
fn negative_control_assets_a_foreign_address_an_inline_script_and_an_event_attribute_are_reported() {
    let foreign = r#"fetch("https://tracker.example.net/p"); const x = "wss://other.example.net/ws";"#;
    let found = route_walk::foreign_urls("/main.js", foreign);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(!route_walk::foreign_urls("/main.js", "import('//cdn.example.net/x.js')").is_empty(), "a protocol-relative address");
    assert!(route_walk::foreign_urls("/icon.svg", r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).is_empty(), "the SVG namespace is allowed");
    assert!(route_walk::foreign_urls("/main.js", r#"x = "http://www.w3.org/2000/svg""#).is_empty(), "the namespace is on the list for scripts too");
    assert!(!route_walk::foreign_urls("/main.js", r#"x = "http://www.w3.org/1999/xhtml""#).is_empty(), "another namespace is not on the list");
    assert!(route_walk::inline_script_problems("<script>alert(1)</script>").iter().any(|p| p.contains("inline")));
    assert!(route_walk::inline_script_problems(r#"<img src="x" onerror="x()">"#).iter().any(|p| p.contains("onerror")));
    assert!(route_walk::inline_script_problems(r#"<script type="module" src="/a.js"></script><meta name="x" content="y">"#).is_empty());
}

#[test]
fn negative_control_routes_an_extra_route_in_a_source_text_and_a_missing_one_are_named() {
    let table = route_walk::table_routes(&route_walk::table());
    let source = r#"
        pub fn parts() -> Vec<Part> {
            let router = Router::new()
                .route("/api/session", get(session))
                .route("/debug", get(debug));
            router
        }
    "#;
    let scanned = route_walk::source_routes(&[("sample.rs", source)]);
    assert_eq!(scanned.literal, ["GET /api/session".to_owned(), "GET /debug".to_owned()].into_iter().collect());
    let compared = route_walk::table_matches_source(&table, &scanned.literal).expect_err("they differ");
    assert!(compared.iter().any(|l| l.contains("in the source and not in the contract's table: GET /debug")), "{compared:?}");
    assert!(compared.iter().any(|l| l.contains("in the contract's table and not in the source: POST /auth/password")));
    // A parameter name does not matter, a method does.
    let renamed = route_walk::source_routes(&[("a.rs", r#".route("/api/admin/accounts/{who}/role", post(role))"#)]);
    assert!(renamed.literal.contains("POST /api/admin/accounts/{}/role"));
    let two = route_walk::source_routes(&[("a.rs", r#".route("/x", get(a).post(b))"#)]);
    assert_eq!(two.literal.len(), 2);
    // A route with a computed path is counted, a route in the test module is not read.
    let dynamic = route_walk::source_routes(&[("a.rs", ".route(path, get(x))\n#[cfg(test)]\nmod t { fn f() { r.route(\"/t\", get(t)) } }")]);
    assert_eq!((dynamic.literal.len(), dynamic.dynamic), (0, 1));
}

#[test]
fn routes_the_table_equals_the_routes_of_the_router_modules() {
    let files = router_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let source = route_walk::source_routes(&refs);
    let table = route_walk::table();
    let from_table = route_walk::table_routes(&table);
    if let Err(lines) = route_walk::table_matches_source(&from_table, &source.literal) {
        panic!("the routes of the source and of the contract differ:\n{}", lines.join("\n"));
    }
    // The files of the page are registered from a table made from the build: two registrations with a computed path.
    assert_eq!(source.dynamic, 2, "the only routes with a computed path are the two groups of page files");
    let by_kind = |kind: Kind| table.iter().filter(|r| r.kind == kind).count();
    println!(
        "Routes: the contract's table has {} rows ({} public, {} session, {} account, {} administrator, {} upload, {} socket, {} device, {} device open); the source has {} literal routes and {} computed",
        table.len(),
        by_kind(Kind::Public),
        by_kind(Kind::Session),
        by_kind(Kind::Fresh),
        by_kind(Kind::Admin),
        by_kind(Kind::Upload),
        by_kind(Kind::Socket),
        by_kind(Kind::Device),
        by_kind(Kind::DeviceOpen),
        source.literal.len(),
        source.dynamic
    );
}

// =============================================================================================================================
// The real process: routes without a cookie, undeclared paths, headers on every answer, the files of the page
// =============================================================================================================================

/// The members an error answer may have. Nothing else: no sentence, no path, no input.
const ERROR_MEMBERS: &[&str] = &["code", "field", "step_up", "retry_seconds"];

/// What is wrong with the body of a refusal, if anything.
fn refusal_body_problems(reply: &Reply) -> Vec<String> {
    if reply.status < 400 || reply.body.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<Value>(&reply.body) else { return vec!["the body of a refusal is not JSON".to_owned()] };
    let Some(object) = value.as_object() else { return vec!["the body of a refusal is not an object".to_owned()] };
    object.keys().filter(|k| !ERROR_MEMBERS.contains(&k.as_str())).map(|k| format!("a member {k} in a refusal")).collect()
}

fn code_of(reply: &Reply) -> String {
    serde_json::from_slice::<Value>(&reply.body).ok().and_then(|v| v["code"].as_str().map(str::to_owned)).unwrap_or_default()
}

/// A request as the page sends it, for a row.
fn probe(client: &Client, row: &Row, id: &str) -> Request {
    let path = route_walk::concrete(row.path, id);
    match row.kind {
        Kind::Upload => {
            let mut request = client.page_request("PUT", &format!("{path}?name=x.bin"), None).header("Content-Type", "application/octet-stream");
            request.body = b"x".to_vec();
            request
        }
        _ if row.changes => client.page_request(row.method, &path, Some(&json!({}))),
        _ => client.page_request(row.method, &path, None),
    }
}

/// The paths no route declares: the forms an attacker or a scanner tries.
fn undeclared_paths() -> Vec<(String, String)> {
    let mut paths: Vec<(String, String)> = [
        ("an administrator page", "/admin"),
        ("a second administrator page", "/administrator"),
        ("a debug page", "/debug"),
        ("a debug api", "/api/debug"),
        ("metrics", "/metrics"),
        ("status", "/status"),
        ("a status api", "/api/status"),
        ("a health path on the network listener", "/healthz"),
        ("a configuration path", "/config"),
        ("a configuration file", "/config.json"),
        ("an api configuration", "/api/config"),
        ("an environment path", "/env"),
        ("an environment file", "/.env"),
        ("an environment api", "/api/environment"),
        ("a source map of the script", "/main.js.map"),
        ("a source map of the login script", "/login.js.map"),
        ("a backup", "/backup"),
        ("a backup archive", "/backup.tar.gz"),
        ("a database file", "/data/holzwave.db"),
        ("the git configuration", "/.git/config"),
        ("the git head", "/.git/HEAD"),
        ("a double slash", "//"),
        ("a double slash before a path", "//admin"),
        ("a double slash inside a path", "/api//session"),
        ("a dot segment", "/a/../admin"),
        ("a leading dot segment", "/./admin"),
        ("an encoded dot segment", "/%2e%2e/admin"),
        ("an encoded slash", "/api%2fsession"),
        ("a trailing slash on a route", "/api/session/"),
        ("a trailing slash on a group", "/api/me/"),
        ("the group of the files", "/files"),
        ("the group of the sign-in routes", "/auth"),
        ("an unknown api", "/api/nope"),
        ("an unknown administrator route", "/api/admin"),
        ("the socket path with a slash", "/ws/"),
        ("a path with a space", "/a%20b"),
        ("a path with a backslash", "/a%5Cb"),
        ("a long path", "/long"),
    ]
    .iter()
    .map(|(what, path)| ((*what).to_owned(), (*path).to_owned()))
    .collect();
    paths.push(("a path of 4 KiB".to_owned(), format!("/{}", "a".repeat(4096))));
    paths
}

/// Calls every row of `rows` as a client with no cookie and no token, and checks what comes back: what the row's kind allows
/// (and for a path of a device feature, what its switch allows: 404 `not_found` while it is off), on every row the same body
/// of an error, no cookie, no `Allow` list, and the answers to the methods the row does not have.
async fn walk_rows(
    client: &mut Client,
    ledger: &Ledger,
    table: &[Row],
    rows: &[&Row],
    feature_on: bool,
    seen: &mut BTreeMap<String, (usize, BTreeSet<u16>)>,
) {
    for row in rows.iter().copied() {
        // `nope` names no provider, so the single sign-on start answers like any unknown name.
        let id = match row.path {
            p if p.contains("oidc") => "nope",
            p if p.contains("{token}") => "0123456789abcdef0123456789abcdef",
            p if p.contains("{handle}") => "AAAAAAAAAAAAAAAA",
            _ => "1",
        };
        let request = probe(client, row, id);
        let reply = client.send(request).await;
        ledger.note(&reply);
        let label = format!("{} {}", row.method, row.path);
        let code = code_of(&reply);
        let wanted = route_walk::unauth_expectation(row, feature_on);
        let allowed =
            wanted.iter().any(|(status, wanted)| *status == reply.status && (wanted.is_empty() || *wanted == code));
        assert!(
            allowed,
            "{label} without a cookie (feature on: {feature_on}) answered {} {code:?}; the contract allows {wanted:?}",
            reply.status
        );
        // A session route under a gate whose feature is off is answered as a path that is not there, before any session is read.
        if row.kind.needs_session() && (feature_on || !route_walk::is_gated(row.path)) {
            assert!(
                reply.all("set-cookie").is_empty() || reply.all("set-cookie").iter().all(|l| l.contains("Max-Age=0")),
                "{label}: no cookie is set"
            );
            assert_eq!(reply.status, 401, "{label}: a session-bound route without a session is 401");
            assert_eq!(code, "invalid", "{label}");
        }
        if route_walk::is_gated(row.path) {
            assert!(reply.all("set-cookie").is_empty(), "{label}: a route of a device feature never sets a cookie");
        }
        assert_eq!(refusal_body_problems(&reply), Vec::<String>::new(), "{label}");
        // Only the explicit public list answers 200.
        if reply.status == 200 {
            assert!(row.kind == Kind::Public, "{label}: a route that is not public answered 200");
        }
        let entry = seen.entry(label.clone()).or_default();
        entry.0 += 1;
        entry.1.insert(reply.status);
        // A wrong method on the same path.
        let methods = route_walk::methods_of(table, row.path);
        for other in route_walk::OTHER_METHODS.iter().filter(|m| !methods.contains(*m)) {
            let path = route_walk::concrete(row.path, id);
            let request = if row.kind == Kind::Upload {
                // The content type of a change is checked before the session; give it the type this path takes.
                client.page_request(other, &path, None).header("Content-Type", "application/octet-stream")
            } else {
                let body = (*other != "GET").then(|| json!({}));
                client.page_request(other, &path, body.as_ref())
            };
            let reply = client.send(request).await;
            ledger.note(&reply);
            // On a route that needs a session or a token the credential check comes first, so a caller without one learns
            // nothing about the methods the path has; on a public route the router answers 405; on the path of a feature that
            // is off the answer is the one of a path that is not there.
            let (status, code) = route_walk::wrong_method_expectation(row, feature_on);
            assert_eq!((reply.status, code_of(&reply).as_str()), (status, code), "{other} on {} without a cookie (feature on: {feature_on})", row.path);
            assert!(reply.header("allow").is_none(), "{other} on {}: the router does not describe itself", row.path);
            assert!(reply.all("set-cookie").is_empty());
            assert_eq!(refusal_body_problems(&reply), Vec::<String>::new());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_a_call_without_a_cookie_gets_only_what_its_kind_allows_and_every_answer_carries_the_headers() {
    let server = Site::new(Some("not-a-secret-just-a-file")).start();
    let mut client = server.client();
    let ledger = Ledger::default();
    let table = route_walk::table();
    let mut seen: BTreeMap<String, (usize, BTreeSet<u16>)> = BTreeMap::new();
    let all: Vec<&Row> = table.iter().collect();
    walk_rows(&mut client, &ledger, &table, &all, false, &mut seen).await;
    // OPTIONS on every path: no success body, no sharing header.
    for row in &table {
        let reply = client.send(client.page_request("OPTIONS", &route_walk::concrete(row.path, "1"), None)).await;
        assert!(reply.status >= 400 && reply.body.len() < 64, "OPTIONS {}: {} {}", row.path, reply.status, reply.text());
        assert!(!reply.names().iter().any(|n| n.starts_with("access-control-")));
    }

    // The paths no route declares.
    let mut statuses: BTreeMap<u16, usize> = BTreeMap::new();
    let corpus = undeclared_paths();
    for (what, path) in &corpus {
        let request = client.page_request("GET", path, None);
        let Some(reply) = client.try_send(request).await else { panic!("{what}: the server closed the connection without an answer") };
        *statuses.entry(reply.status).or_default() += 1;
        assert_eq!(reply.status, 404, "{what} ({}): {}", &path[..path.len().min(20)], reply.text());
        assert_eq!(code_of(&reply), "not_found", "{what}");
        assert_eq!(refusal_body_problems(&reply), Vec::<String>::new(), "{what}");
        assert!(!reply.text().contains(path.as_str()), "{what}: the answer does not echo the path");
        assert!(reply.all("set-cookie").is_empty());
    }
    // A path with a NUL byte cannot be told apart from a malformed request line: it is refused, with no detail.
    let mut nul = Request::new("GET", "/a\0b").close();
    nul.headers.push(("User-Agent".to_owned(), "t".to_owned()));
    let nul_reply = client.try_send(nul).await;
    let nul_status = nul_reply.as_ref().map(|r| r.status);
    assert!(matches!(nul_status, None | Some(400 | 404)), "a NUL in the path: {nul_status:?}");
    // The HTTP parser answers a malformed request line itself, before any request exists for the application's layers: the
    // answer has no body, no cookie and no application header, and it is listed as a limit of the header check below.
    if let Some(reply) = &nul_reply {
        assert!(reply.body.is_empty() && reply.all("set-cookie").is_empty(), "the parser's refusal has no body and no cookie");
        assert!(reply.names().iter().all(|n| ["connection", "content-length", "date"].contains(&n.as_str())), "{:?}", reply.names());
    }

    // Every answer of the walk, whatever its status, carries the headers of the contract.
    let exchanges = server.recorder.exchanges();
    let mut by_status: BTreeSet<u16> = BTreeSet::new();
    let mut problems = Vec::new();
    for exchange in exchanges.iter().filter(|e| !e.target.contains('\0')) {
        by_status.insert(exchange.status);
        let path = exchange.target.split('?').next().unwrap_or_default();
        problems.extend(route_walk::check_headers(&Seen { status: exchange.status, path, headers: &exchange.headers }, HOST));
    }
    assert!(problems.is_empty(), "headers missing on some answers:\n{}", problems.join("\n"));
    println!("Routes (real process, no cookie): {} rows walked, {} answers; the corpus of undeclared paths has {} entries plus a NUL path ({nul_status:?}), statuses {statuses:?}", table.len(), exchanges.len(), corpus.len());
    println!("Headers: {} answers checked for {} fixed headers, the policy and the resource policy; statuses seen {by_status:?}", exchanges.len(), route_walk::FIXED_HEADERS.len());
    for (label, (count, statuses)) in &seen {
        println!("  {label}: {count} call, {statuses:?}");
    }
    let table_len = table.len();
    println!("Routes: printed total {table_len} equals the table's length {}", route_walk::table().len());
    let cookie_problems = ledger.problems.lock().expect("lock").clone();
    assert!(cookie_problems.is_empty(), "cookies with a wrong flag:\n{}", cookie_problems.join("\n"));
}

// ---- the files of the page ----------------------------------------------------------------------------------------------------

/// The files the sign-in page needs: the files the contract names and the modules the sign-in script imports, followed in the
/// build.
fn pre_auth_set(paths: &[String], dist: &Path) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = route_walk::PRE_AUTH_FILES.iter().map(|s| (*s).to_owned()).collect();
    let mut queue = vec!["login.js".to_owned()];
    let mut done: BTreeSet<String> = BTreeSet::new();
    while let Some(name) = queue.pop() {
        if !done.insert(name.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dist.join(&name)) else { continue };
        for quote in ['"', '\''] {
            for marker in ["from ", "import "] {
                let needle = format!("{marker}{quote}./");
                let mut from = 0;
                while let Some(at) = text[from..].find(&needle) {
                    let start = from + at + needle.len();
                    if let Some(end) = text[start..].find(quote) {
                        let module = &text[start..start + end];
                        if module.ends_with(".js") {
                            set.insert(format!("/{module}"));
                            queue.push(module.to_owned());
                        }
                    }
                    from = start;
                }
            }
        }
    }
    set.into_iter().filter(|p| paths.contains(p)).chain(route_walk::PRE_AUTH_FILES.iter().map(|s| (*s).to_owned())).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn assets_every_served_file_is_fetched_over_tls_and_none_names_an_address_outside_the_origin() {
    let server = Site::new(None).start();
    let mut anonymous = server.client();
    let mut admin = server.client();
    let token = server.setup_token();
    admin.enrol("/auth/bootstrap", &token, "alice", PASSWORD).await;
    let assets = holzwave_server::http::embed::Assets::embedded();
    let paths: Vec<String> = assets.paths().map(str::to_owned).collect();
    assert!(paths.len() > 50, "the build holds a page: {} files", paths.len());
    let dist = repo_root().join("app/dist");
    let public = pre_auth_set(&paths, &dist);
    let mut foreign: Vec<String> = Vec::new();
    let mut inline: Vec<String> = Vec::new();
    let (mut scanned, mut bytes, mut open, mut closed) = (0usize, 0usize, 0usize, 0usize);
    let mut allowed_hits: BTreeSet<(String, String)> = BTreeSet::new();
    for path in &paths {
        let without = anonymous.send(anonymous.page_request("GET", path, None)).await;
        let is_public = public.contains(path) || path.starts_with(route_walk::PRE_AUTH_PREFIX);
        if is_public {
            assert_eq!(without.status, 200, "{path} is a file of the sign-in page and needs no session");
            open += 1;
        } else {
            assert_eq!(without.status, 401, "{path} needs a session: {}", without.status);
            assert!(without.body.len() < 64, "{path}: a refusal carries no file");
            closed += 1;
        }
        let with = admin.send(admin.page_request("GET", path, None)).await;
        assert_eq!(with.status, 200, "{path} with a session");
        let text = String::from_utf8_lossy(&with.body).into_owned();
        let kind = path.rsplit('.').next().unwrap_or_default();
        if ["js", "html", "css", "svg"].contains(&kind) {
            scanned += 1;
            bytes += with.body.len();
            for literal in route_walk::foreign_urls(path, &text) {
                foreign.push(format!("{path}: {literal}"));
            }
            for literal in route_walk::url_literals(&text) {
                if let Some((file, start, reason)) = route_walk::URL_ALLOWED.iter().find(|(file, start, _)| path.contains(file) && literal.starts_with(start)) {
                    allowed_hits.insert(((*file).to_owned(), format!("{start}: {reason}")));
                }
            }
        }
        if kind == "html" {
            inline.extend(route_walk::inline_script_problems(&text).into_iter().map(|p| format!("{path}: {p}")));
        }
        // The policy of every page file is the contract's.
        let csp = with.header("content-security-policy").unwrap_or_default();
        assert!(route_walk::check_csp(csp, HOST).is_empty(), "{path}: {:?}", route_walk::check_csp(csp, HOST));
    }
    assert!(foreign.is_empty(), "served files name an address outside the origin:\n{}", foreign.join("\n"));
    assert!(inline.is_empty(), "inline scripts or event attributes:\n{}", inline.join("\n"));
    println!("Assets: {} served files fetched over TLS ({open} open, {closed} behind a session), {scanned} scripts, pages, styles and images scanned ({bytes} bytes), no foreign address and no inline script", paths.len());
    for (file, what) in allowed_hits {
        println!("  allowed in files like {file}: {what}");
    }
    // Every cookie the enrolment and the walk set has the flags of the contract.
    let mut cookies: BTreeSet<String> = BTreeSet::new();
    for exchange in server.recorder.exchanges() {
        for (name, value) in &exchange.headers {
            if name.eq_ignore_ascii_case("set-cookie") {
                let problems = route_walk::check_set_cookie(value);
                assert!(problems.is_empty(), "{problems:?}");
                cookies.insert(redact(value));
            }
        }
    }
    for cookie in &cookies {
        println!("Cookies (real process): {cookie}");
    }
    assert!(cookies.iter().any(|c| c.starts_with("__Host-hw_session=")) && cookies.iter().any(|c| c.starts_with("__Host-hw_pending=")), "the enrolment set both cookies");
    let header = admin.send(admin.page_request("GET", "/", None)).await.header("content-security-policy").unwrap_or_default().to_owned();
    println!("Policy: the header as served: {header}");
    println!("Policy: directive by directive equal to the contract: {:?}", route_walk::parse_csp(&header) == route_walk::contract_policy(HOST));
    // The sign-in page carries the same headers as the application.
    assert!(route_walk::check_headers(&Seen { status: 200, path: "/", headers: &anonymous.send(anonymous.page_request("GET", "/", None)).await.headers }, HOST).is_empty());
}

/// A site whose two device switches are on from its first moment: the store is written before the process starts.
fn site_with_the_device_features_on() -> Site {
    let site = Site::new(Some("not-a-secret-just-a-file"));
    {
        let store = holzwave_server::store::Store::open(&site.data_dir()).expect("the store of the site opens");
        store
            .write(|tx| {
                tx.setting_set("sync", true)?;
                tx.setting_set("remote", true)
            })
            .expect("the switches are written");
    }
    site
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_a_row_under_a_gated_prefix_answers_its_kinds_refusal_while_the_feature_is_on() {
    let server = site_with_the_device_features_on().start();
    let mut client = server.client();
    let ledger = Ledger::default();
    let table = route_walk::table();
    let gated: Vec<&Row> = table.iter().filter(|r| route_walk::is_gated(r.path)).collect();
    assert!(!gated.is_empty(), "the table has rows under a gated prefix");
    let mut seen: BTreeMap<String, (usize, BTreeSet<u16>)> = BTreeMap::new();
    walk_rows(&mut client, &ledger, &table, &gated, true, &mut seen).await;
    // The same rows are not 404 now: the gate is open and the credential is what refuses.
    for row in &gated {
        let (statuses, label) = (&seen[&format!("{} {}", row.method, row.path)].1, format!("{} {}", row.method, row.path));
        assert!(!statuses.contains(&404), "{label}: with the feature on the path exists: {statuses:?}");
    }
    let problems: Vec<String> = server
        .recorder
        .exchanges()
        .iter()
        .flat_map(|e| route_walk::check_headers(&Seen { status: e.status, path: e.target.split('?').next().unwrap_or_default(), headers: &e.headers }, HOST))
        .collect();
    assert!(problems.is_empty(), "headers missing on some answers:\n{}", problems.join("\n"));
    let cookie_problems = ledger.problems.lock().expect("lock").clone();
    assert!(cookie_problems.is_empty(), "cookies with a wrong flag:\n{}", cookie_problems.join("\n"));
    println!("Routes (real process, features on): {} rows under a gated prefix walked, statuses {:?}", gated.len(), seen.values().flat_map(|(_, s)| s.iter().copied()).collect::<BTreeSet<_>>());
}

#[test]
fn routes_every_row_of_a_device_kind_is_under_a_gated_prefix_and_the_oracle_lists_the_prefixes_itself() {
    let table = route_walk::table();
    for row in table.iter().filter(|r| matches!(r.kind, Kind::Device | Kind::DeviceOpen)) {
        assert!(route_walk::is_gated(row.path), "{} {} has the kind of a device route and no gated prefix", row.method, row.path);
    }
    // The three rows of the device mounts, with the kinds the contract gives them.
    let kind_of = |method: &str, path: &str| table.iter().find(|r| (r.method, r.path) == (method, path)).map(|r| r.kind);
    assert_eq!(kind_of("GET", "/api/device/me"), Some(Kind::Device));
    assert_eq!(kind_of("GET", "/api/admin/device-access"), Some(Kind::Admin));
    assert_eq!(kind_of("POST", "/api/admin/device-access"), Some(Kind::Admin));
    assert!(!route_walk::is_gated("/api/admin/device-access"), "the switch routes are not behind the gate they control");
    // The oracle's prefixes are written in its own file, not read from the code under test.
    assert_eq!(route_walk::GATED_PREFIXES, ["/api/device/", "/sync/v1/", "/ws/device"]);
    // The source of the router modules mounts only gated paths as a device kind.
    let files = router_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    assert_eq!(route_walk::device_route_problems(&refs), Vec::<String>::new());
    assert!(
        files.iter().any(|(_, text)| text.contains("access: Access::Device")),
        "the check has a device mount to look at, so a green run is not an empty one"
    );
}

#[test]
fn negative_control_routes_a_device_route_without_a_gated_prefix_is_named() {
    let ungated = r#"
        pub fn parts() -> Vec<Part> {
            let device = Router::new().route("/api/other/me", get(me));
            vec![Part { access: Access::Device, router: device }]
        }
    "#;
    let problems = route_walk::device_route_problems(&[("a.rs", ungated)]);
    assert!(problems.iter().any(|p| p.contains("GET /api/other/me") && p.contains("not under a gated prefix")), "{problems:?}");
    // The kind that has no credential is checked too, with the router written inside the part.
    let open = r#"vec![Part { access: Access::DeviceOpen, router: Router::new().route("/api/other/code", post(code)) }]"#;
    let problems = route_walk::device_route_problems(&[("b.rs", open)]);
    assert!(problems.iter().any(|p| p.contains("POST /api/other/code") && p.contains("DeviceOpen")), "{problems:?}");
    // A prefix is whole segments: a path that only starts with the same letters is not gated.
    let near = r#"let d = Router::new().route("/api/devices/me", get(me)); vec![Part { access: Access::Device, router: d }]"#;
    assert!(!route_walk::device_route_problems(&[("c.rs", near)]).is_empty());
    // A router the check cannot read is a complaint, not a pass.
    let hidden = "vec![Part { access: Access::Device, router: built_elsewhere() }]";
    assert!(!route_walk::device_route_problems(&[("d.rs", hidden)]).is_empty());
    // The shapes that are right pass: every gated prefix, and a part of another kind is not looked at.
    let good = r#"
        let device = Router::new().route("/api/device/me", get(me)).route("/sync/v1/list/{dir}", get(l)).route("/ws/device", get(w));
        vec![Part { access: Access::Device, router: device }, Part { access: Access::Public, router: Router::new().route("/x", get(x)) }]
    "#;
    assert_eq!(route_walk::device_route_problems(&[("e.rs", good)]), Vec::<String>::new());
    // A route in the test module is not read.
    let tests_only = "#[cfg(test)]\nmod t { fn f() { Part { access: Access::Device, router: Router::new().route(\"/x\", get(x)) }; } }";
    assert_eq!(route_walk::device_route_problems(&[("f.rs", tests_only)]), Vec::<String>::new());
}

#[test]
fn negative_control_routes_a_gated_row_is_expected_to_be_404_while_off_and_the_kinds_refusal_while_on() {
    let table = route_walk::table();
    let device = table.iter().find(|r| r.path == "/api/device/me").expect("the device row");
    assert_eq!(route_walk::unauth_expectation(device, false), [(404, "not_found")]);
    assert_eq!(route_walk::unauth_expectation(device, true), [(401, "invalid")]);
    assert_eq!(route_walk::wrong_method_expectation(device, false), (404, "not_found"));
    assert_eq!(route_walk::wrong_method_expectation(device, true), (401, "invalid"));
    // A row outside the gates keeps what the table says, whatever the switch.
    let admin = table.iter().find(|r| r.path == "/api/admin/device-access" && r.method == "POST").expect("the admin row");
    assert_eq!(route_walk::unauth_expectation(admin, false), route_walk::unauth_expectation(admin, true));
    assert_eq!(route_walk::unauth_expectation(admin, false), [(401, "invalid")]);
    let session = table.iter().find(|r| r.path == "/api/session").expect("a public row");
    assert_eq!(route_walk::unauth_expectation(session, false), [(200, "")]);
    assert_eq!(route_walk::wrong_method_expectation(session, false), (405, "bad_request"));
    // The prefix is a whole segment.
    for (path, gated) in [("/api/device/me", true), ("/api/device", true), ("/api/devices", false), ("/sync/v1/blob/files/x", true), ("/sync/v2", false), ("/ws/device", true), ("/ws", false)] {
        assert_eq!(route_walk::is_gated(path), gated, "{path}");
    }
}

// =============================================================================================================================
// The in-process assembly: sessions of a given age, many throwaway people, the cross-site matrix
// =============================================================================================================================

use edge::Signed;
use unlock_env::webauthn_client::SoftPasskey;

/// The members of the sessions' answers a refusal may have.
fn body_code(reply: &Reply) -> String {
    code_of(reply)
}

/// The in-process rig: the assembly, an administrator, a counter for unique names, and what the run saw.
struct Rig {
    env: UnlockEnv,
    admin: User,
    bob: User,
    bob_session: Signed,
    admin_first: Signed,
    counter: std::sync::atomic::AtomicUsize,
    ledger: Ledger,
    header_problems: std::sync::Mutex<Vec<String>>,
    answers: std::sync::atomic::AtomicUsize,
}

/// A request the matrix may alter, and what to watch before and after it.
struct Case {
    parts: Parts,
    success: u16,
    /// The cookie a correct call sets, if any.
    sets_cookie: Option<&'static str>,
    /// Whose state is watched: the administrator's view of the list, the person's own page, the upload folder.
    watch: Watch,
}

#[derive(Default)]
struct Watch {
    admin: Option<Signed>,
    person: Option<(User, Signed)>,
    uploads: Option<PathBuf>,
    /// Whether the state of the two device switches is watched (it is read with the administrator's session).
    switches: bool,
}

impl Rig {
    async fn start() -> Rig {
        let env = UnlockEnv::start().await;
        let (admin, admin_first, bob, bob_session) = env.people().await;
        Rig {
            env,
            admin,
            bob,
            bob_session,
            admin_first,
            counter: std::sync::atomic::AtomicUsize::new(0),
            ledger: Ledger::default(),
            header_problems: std::sync::Mutex::new(Vec::new()),
            answers: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn next_name(&self, stem: &str) -> String {
        format!("{stem}{}", self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1)
    }

    /// A session of the administrator that signed in just now (the clock moves 31 seconds for the authenticator's next code).
    async fn admin_now(&self) -> Signed {
        self.env.sign_in(&self.admin).await
    }

    /// A new member and the session of the enrolment.
    async fn member(&self) -> (User, Signed) {
        let name = self.next_name("m");
        let (user, signed, _) = self.env.invite(&self.admin, &name, "a new horse battery staple").await;
        (user, signed)
    }

    /// The request a page sends for `who` (no cookie and no token when `None`).
    fn page(&self, who: Option<&Signed>, method: &str, target: &str, body: Option<&Value>) -> Parts {
        let mut parts = Parts {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: vec![
                ("Host".to_owned(), HOST.to_owned()),
                ("User-Agent".to_owned(), "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0".to_owned()),
                ("Connection".to_owned(), "close".to_owned()),
            ],
            body: Vec::new(),
        };
        if method != "GET" {
            parts.set("origin", ORIGIN);
            parts.set("content-type", "application/json");
        }
        if let Some(who) = who {
            parts.set("cookie", &format!("__Host-hw_session={}", who.cookie));
            if method != "GET" {
                parts.set("x-csrf-token", &who.csrf);
            }
        }
        if let Some(body) = body {
            parts.body = serde_json::to_vec(body).expect("json");
        }
        parts
    }

    async fn send(&self, parts: &Parts) -> Reply {
        let mut request = Request::new(&parts.method, &parts.target);
        request.headers = parts.headers.clone();
        request.body = parts.body.clone();
        let reply = self.env.ws.plain(request).await;
        self.ledger.note(&reply);
        self.answers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parts.target.split('?').next().unwrap_or_default();
        let problems = route_walk::check_headers(&Seen { status: reply.status, path, headers: &reply.headers }, HOST);
        if !problems.is_empty() {
            self.header_problems.lock().expect("lock").extend(problems);
        }
        reply
    }

    /// Everything a refused change must leave alone: the audit, the list the administrator sees, the person's own page, the
    /// store state, the uploads.
    async fn fingerprint(&self, watch: &Watch) -> String {
        let mut text = self.env.audit_text();
        if let Some(admin) = &watch.admin {
            text.push_str(&self.send(&self.page(Some(admin), "GET", "/api/admin/accounts", None)).await.text());
            if watch.switches {
                text.push_str(&self.send(&self.page(Some(admin), "GET", "/api/admin/device-access", None)).await.text());
            }
        }
        if let Some((user, signed)) = &watch.person {
            text.push_str(&self.send(&self.page(Some(signed), "GET", "/api/me", None)).await.text());
            text.push_str(&format!("{:?}{}", self.env.state(user), self.env.live_sessions(user)));
        }
        if let Some(dir) = &watch.uploads {
            let mut names: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            names.sort();
            text.push_str(&names.join(","));
        }
        text
    }

    /// A password person who also has a second way in, a passkey: the id of the passkey and its authenticator.
    async fn add_passkey(&self, user: &User, signed: &Signed) -> (String, SoftPasskey) {
        let begin = self.send(&self.page(Some(signed), "POST", "/api/me/passkeys", Some(&json!({"step": "begin", "name": "phone"})))).await;
        assert_eq!(begin.status, 200, "{}", begin.text());
        let mut authenticator = SoftPasskey::new();
        let origin = webauthn_rs::prelude::Url::parse(ORIGIN).expect("the origin");
        let credential = authenticator.register(&begin.json()["options"], &origin).expect("the authenticator answers");
        let finish = self.send(&self.page(Some(signed), "POST", "/api/me/passkeys", Some(&json!({"step": "finish", "credential": credential})))).await;
        assert_eq!(finish.status, 200, "{}: {}", user.name, finish.text());
        let id = finish.json()["id"].as_i64().map(|i| i.to_string()).or_else(|| finish.json()["id"].as_str().map(str::to_owned)).expect("an id");
        (id, authenticator)
    }

    /// A grant an app started, as the user code and the device code. Open grants are counted per address and all the requests of
    /// the rig come from one, so the clock first moves past the end of the grants the earlier rows left (a grant lives ten
    /// minutes); the caller makes its own session after this call, because the move also ages every sign-in.
    async fn grant(&self) -> (String, String) {
        self.env.advance(holzwave_server::device::GRANT_TTL + 1);
        let body = json!({"name": "Matrix laptop", "platform": "linux"});
        let reply = self.send(&self.page(None, "POST", "/api/device/code", Some(&body))).await;
        assert_eq!(reply.status, 200, "a grant is started: {}", reply.text());
        let made = reply.json();
        (
            made["user_code"].as_str().expect("a user code").to_owned(),
            made["device_code"].as_str().expect("a device code").to_owned(),
        )
    }

    /// A new member with one device, through the whole grant: the person, the session that approved it, the device's id and its
    /// token.
    async fn device_of_a_member(&self) -> (User, Signed, i64, String) {
        let (user_code, device_code) = self.grant().await;
        let (user, signed) = self.member().await;
        let approved = self
            .send(&self.page(Some(&signed), "POST", "/api/device/approve", Some(&json!({"code": user_code}))))
            .await;
        assert_eq!(approved.status, 204, "the grant is approved: {}", approved.text());
        let polled =
            self.send(&self.page(None, "POST", "/api/device/token", Some(&json!({"device_code": device_code})))).await;
        let body = polled.json();
        assert_eq!(body["state"], "approved", "the device is made: {}", polled.text());
        (
            user,
            signed,
            body["device_id"].as_i64().expect("a device id"),
            body["token"].as_str().expect("a token").to_owned(),
        )
    }

    /// A correct request for a row on a new target, with what to watch. `None` for a row the rig cannot make a case for.
    async fn case_for(&self, row: &Row) -> Case {
        let env = &self.env;
        let plain = |parts: Parts, success: u16, watch: Watch| Case { parts, success, sets_cookie: None, watch };
        match (row.method, row.path) {
            // ---- the sign-in routes: no session ----
            ("POST", "/auth/password") => {
                let (user, _) = self.member().await;
                let parts = self.page(None, "POST", row.path, Some(&json!({"username": user.name, "password": user.password})));
                Case { parts, success: 200, sets_cookie: Some("__Host-hw_pending"), watch: Watch { admin: Some(self.admin_now().await), ..Watch::default() } }
            }
            ("POST", "/auth/totp") => {
                let watch = Watch { admin: Some(self.admin_now().await), ..Watch::default() };
                let (user, _) = self.member().await;
                env.advance(unlock_env::CODE_STEP);
                let begin = self.send(&self.page(None, "POST", "/auth/password", Some(&json!({"username": user.name, "password": user.password})))).await;
                assert_eq!(begin.status, 200, "{}", begin.text());
                let pending = begin
                    .all("set-cookie")
                    .iter()
                    .find_map(|c| c.strip_prefix("__Host-hw_pending=").map(|r| r.split(';').next().unwrap_or_default().to_owned()))
                    .expect("a pending cookie");
                let code = real_process::totp_code(&user.secret, env.now());
                let mut parts = self.page(None, "POST", row.path, Some(&json!({"code": code})));
                parts.set("cookie", &format!("__Host-hw_pending={pending}"));
                Case { parts, success: 200, sets_cookie: Some("__Host-hw_session"), watch }
            }
            ("POST", "/auth/passkey/begin") => {
                plain(self.page(None, "POST", row.path, Some(&json!({}))), 200, Watch { admin: Some(self.admin_now().await), ..Watch::default() })
            }
            ("POST", "/auth/passkey/finish") => {
                let name = self.next_name("k");
                let (user, _, _) = env.invite_with_passkey(&self.admin, &name, None).await;
                let begin = self.send(&self.page(None, "POST", "/auth/passkey/begin", Some(&json!({})))).await;
                assert_eq!(begin.status, 200, "{}", begin.text());
                let pending = begin
                    .all("set-cookie")
                    .iter()
                    .find_map(|c| c.strip_prefix("__Host-hw_pending=").map(|r| r.split(';').next().unwrap_or_default().to_owned()))
                    .expect("a pending cookie");
                let origin = webauthn_rs::prelude::Url::parse(ORIGIN).expect("the origin");
                let credential = user.passkey.as_ref().expect("a passkey").lock().expect("lock").authenticate(&begin.json()["options"], &origin).expect("the authenticator answers");
                let mut parts = self.page(None, "POST", row.path, Some(&json!({"credential": credential})));
                parts.set("cookie", &format!("__Host-hw_pending={pending}"));
                Case { parts, success: 200, sets_cookie: Some("__Host-hw_session"), watch: Watch { admin: Some(self.admin_now().await), ..Watch::default() } }
            }
            ("POST", "/auth/invite/redeem") => {
                let token = env.accounts.invite_create(self.admin.id, None, None, holzwave_server::store::Role::Member).expect("an invitation").token.expose().to_owned();
                let name = self.next_name("r");
                let body = json!({"step": "begin", "token": token, "username": name, "display_name": name, "method": "password", "password": "a new horse battery staple"});
                Case { parts: self.page(None, "POST", row.path, Some(&body)), success: 200, sets_cookie: Some("__Host-hw_pending"), watch: Watch { admin: Some(self.admin_now().await), ..Watch::default() } }
            }
            // ---- a session ----
            ("POST", "/auth/logout") => {
                let (user, signed) = self.member().await;
                plain(self.page(Some(&signed), "POST", row.path, Some(&json!({}))), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/auth/unlock") => {
                let (user, signed) = self.member().await;
                env.lock_at_the_worker(&user).await;
                env.wait_state(&user, holzwave_server::http::app::StoreState::Locked, "the store to lock").await;
                let parts = self.page(Some(&signed), "POST", row.path, Some(&json!({"recovery_code": user.recovery})));
                plain(parts, 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/auth/step-up") => {
                let (user, signed) = self.member().await;
                let parts = self.page(Some(&signed), "POST", row.path, Some(&json!({"password": user.password})));
                plain(parts, 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/auth/step-up/begin") => {
                let name = self.next_name("k");
                let (user, signed, _) = env.invite_with_passkey(&self.admin, &name, None).await;
                plain(self.page(Some(&signed), "POST", row.path, Some(&json!({}))), 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("PUT", "/api/upload") => {
                let (user, signed) = self.member().await;
                let mut parts = self.page(Some(&signed), "PUT", "/api/upload?name=matrix.bin", None);
                parts.set("content-type", "application/octet-stream");
                parts.body = b"matrix bytes".to_vec();
                let uploads = env.home(&user).join("uploads");
                plain(parts, 200, Watch { person: Some((user, signed)), uploads: Some(uploads), ..Watch::default() })
            }
            // ---- the person's own account ----
            ("POST", "/api/me/password") => {
                let (user, signed) = self.member().await;
                let body = json!({"current": user.password, "new": "an even newer horse battery"});
                plain(self.page(Some(&signed), "POST", row.path, Some(&body)), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/api/me/totp") => {
                let (user, signed) = self.member().await;
                plain(self.page(Some(&signed), "POST", row.path, Some(&json!({"step": "begin"}))), 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/api/me/passkeys") => {
                let (user, signed) = self.member().await;
                let body = json!({"step": "begin", "name": "phone"});
                plain(self.page(Some(&signed), "POST", row.path, Some(&body)), 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("DELETE", "/api/me/passkeys/{id}") => {
                let (user, signed) = self.member().await;
                let (id, _) = self.add_passkey(&user, &signed).await;
                let target = route_walk::concrete(row.path, &id);
                plain(self.page(Some(&signed), "DELETE", &target, None), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("PUT", "/api/me/passphrase") => {
                let (user, signed) = self.member().await;
                let body = json!({"passphrase": "a long passphrase for my store"});
                plain(self.page(Some(&signed), "PUT", row.path, Some(&body)), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("POST", "/api/me/recovery") => {
                let (user, signed) = self.member().await;
                plain(self.page(Some(&signed), "POST", row.path, Some(&json!({}))), 200, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("DELETE", "/api/me/sessions") => {
                let (user, signed) = self.member().await;
                plain(self.page(Some(&signed), "DELETE", row.path, None), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("DELETE", "/api/me/sessions/{handle}") => {
                let (user, signed) = self.member().await;
                // A second session of the same person, ended by the first.
                let second = env.sign_in(&user).await;
                let me = self.send(&self.page(Some(&signed), "GET", "/api/me", None)).await.json();
                let other = me["sessions"].as_array().expect("sessions").iter().find(|s| s["current"] == json!(false)).expect("the second session")["id"]
                    .as_str()
                    .expect("a handle")
                    .to_owned();
                drop(second);
                let target = route_walk::concrete(row.path, &other);
                plain(self.page(Some(&signed), "DELETE", &target, None), 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            // ---- the administrator ----
            ("POST", "/api/admin/invitations") => {
                let admin = self.admin_now().await;
                plain(self.page(Some(&admin), "POST", row.path, Some(&json!({"role": "member", "days": 1}))), 200, Watch { admin: Some(admin), ..Watch::default() })
            }
            ("POST", "/api/admin/device-access") => {
                let admin = self.admin_now().await;
                plain(
                    self.page(Some(&admin), "POST", row.path, Some(&json!({"remote": true}))),
                    200,
                    Watch { admin: Some(admin), switches: true, ..Watch::default() },
                )
            }
            // ---- the sign-in of an app, and the revocation of a device (the remote switch is on from the row above) ----
            ("POST", "/api/device/code") => {
                let body = json!({"name": "Matrix laptop", "platform": "linux"});
                plain(self.page(None, "POST", row.path, Some(&body)), 200, Watch::default())
            }
            ("POST", "/api/device/token") => {
                let (_, device_code) = self.grant().await;
                plain(
                    self.page(None, "POST", row.path, Some(&json!({"device_code": device_code}))),
                    200,
                    Watch::default(),
                )
            }
            ("POST", "/api/device/lookup" | "/api/device/approve" | "/api/device/deny") => {
                let (user_code, _) = self.grant().await;
                let (user, signed) = self.member().await;
                let success = if row.path.ends_with("/lookup") { 200 } else { 204 };
                plain(
                    self.page(Some(&signed), "POST", row.path, Some(&json!({"code": user_code}))),
                    success,
                    Watch { person: Some((user, signed)), ..Watch::default() },
                )
            }
            ("DELETE", "/api/device/me") => {
                let (user, signed, _, token) = self.device_of_a_member().await;
                let mut parts = self.page(None, "DELETE", row.path, None);
                parts.set("authorization", &format!("Bearer {token}"));
                plain(parts, 204, Watch { person: Some((user, signed)), ..Watch::default() })
            }
            ("DELETE", "/api/me/devices/{id}") => {
                let (user, signed, id, _) = self.device_of_a_member().await;
                let target = route_walk::concrete(row.path, &id.to_string());
                plain(
                    self.page(Some(&signed), "DELETE", &target, None),
                    204,
                    Watch { person: Some((user, signed)), ..Watch::default() },
                )
            }
            ("DELETE", "/api/admin/devices/{id}") => {
                let (user, signed, id, _) = self.device_of_a_member().await;
                let admin = self.admin_now().await;
                let target = route_walk::concrete(row.path, &id.to_string());
                plain(
                    self.page(Some(&admin), "DELETE", &target, None),
                    204,
                    Watch { admin: Some(admin), person: Some((user, signed)), ..Watch::default() },
                )
            }
            (method, path) if path.starts_with("/api/admin/accounts/{id}") => {
                let (target, target_session) = self.member().await;
                let admin = self.admin_now().await;
                let url = route_walk::concrete(path, &target.id.get().to_string());
                let body = match path.rsplit('/').next() {
                    Some("role") => Some(json!({"role": "admin"})),
                    Some("{id}") => None,
                    _ => Some(json!({})),
                };
                if path.ends_with("/enable") {
                    // Enabling an account that is active is a no-op the contract accepts; disable it first so the call changes something.
                    let off = self.send(&self.page(Some(&admin), "POST", &format!("/api/admin/accounts/{}/disable", target.id.get()), Some(&json!({})))).await;
                    assert_eq!(off.status, 204, "{}", off.text());
                }
                plain(
                    self.page(Some(&admin), method, &url, body.as_ref()),
                    204,
                    Watch { admin: Some(admin), person: if path.ends_with("/enable") { None } else { Some((target, target_session)) }, ..Watch::default() },
                )
            }
            other => panic!("no case for {other:?}: the matrix must cover every changing row of the table"),
        }
    }
}

/// Sends `parts` to an environment.
async fn send_on(env: &UnlockEnv, parts: &Parts) -> Reply {
    let mut request = Request::new(&parts.method, &parts.target);
    request.headers = parts.headers.clone();
    request.body = parts.body.clone();
    env.ws.plain(request).await
}

/// What the matrix saw of one variation: the rows it ran on and the statuses it got.
#[derive(Default)]
struct Tally {
    ran: BTreeMap<String, (Vec<String>, BTreeSet<u16>)>,
    skipped: BTreeMap<String, Vec<String>>,
}

impl Tally {
    fn ran(&mut self, variation: &str, row: &str, status: u16) {
        let entry = self.ran.entry(variation.to_owned()).or_default();
        entry.0.push(row.to_owned());
        entry.1.insert(status);
    }
}

fn row_label(row: &Row) -> String {
    format!("{} {}", row.method, row.path)
}

/// What is wrong with a refusal: the status, the code, a cookie, a member that gives a reason.
fn refusal_problems(reply: &Reply, status: u16, code: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if (reply.status, body_code(reply).as_str()) != (status, code) {
        problems.push(format!("the call is refused with {status} {code}, got {} {}", reply.status, body_code(reply)));
    }
    if !reply.all("set-cookie").is_empty() {
        problems.push("a refusal sets a cookie".to_owned());
    }
    problems.extend(refusal_body_problems(reply));
    problems
}

fn assert_refused(label: &str, variation: &str, reply: &Reply, status: u16, code: &str) {
    let problems = refusal_problems(reply, status, code);
    assert!(problems.is_empty(), "{label} with {variation}: {}", problems.join("; "));
}

/// The state a refused change must leave alone, compared as text.
fn unchanged(before: &str, after: &str) -> bool {
    before == after
}

#[test]
fn negative_control_matrix_a_success_a_wrong_class_a_cookie_and_a_changed_state_are_reported() {
    let reply = |status: u16, body: &str, headers: Vec<(&str, &str)>| Reply {
        status,
        headers: headers.into_iter().map(|(n, v)| (n.to_owned(), v.to_owned())).collect(),
        body: body.as_bytes().to_vec(),
    };
    assert!(refusal_problems(&reply(403, r#"{"code":"forbidden"}"#, vec![]), 403, "forbidden").is_empty(), "a correct refusal passes");
    assert!(!refusal_problems(&reply(204, "", vec![]), 403, "forbidden").is_empty(), "a success where a refusal is due");
    assert!(!refusal_problems(&reply(200, "{}", vec![]), 401, "invalid").is_empty());
    assert!(!refusal_problems(&reply(400, r#"{"code":"bad_request"}"#, vec![]), 403, "forbidden").is_empty(), "a wrong class");
    let with_cookie = reply(403, r#"{"code":"forbidden"}"#, vec![("set-cookie", "__Host-hw_session=A; Secure")]);
    assert!(refusal_problems(&with_cookie, 403, "forbidden").iter().any(|p| p.contains("cookie")), "a cookie on a refusal");
    let detail = reply(403, r#"{"code":"forbidden","reason":"the token is wrong"}"#, vec![]);
    assert!(refusal_problems(&detail, 403, "forbidden").iter().any(|p| p.contains("reason")), "a member that gives a reason");
    assert!(unchanged("a", "a") && !unchanged("a", "b"), "a changed state is reported");
}

impl Rig {
    /// The positive control, then every variation on a fresh target, then the correct call once more on that target.
    async fn run_row(&self, row: &Row, tally: &mut Tally) {
        let label = row_label(row);
        // The positive control: a correct call on a throwaway target works.
        let control = self.case_for(row).await;
        let reply = self.send(&control.parts).await;
        assert_eq!(reply.status, control.success, "{label}: the positive control: {} {}", reply.status, reply.text().chars().take(120).collect::<String>());
        if let Some(name) = control.sets_cookie {
            assert!(reply.all("set-cookie").iter().any(|c| c.starts_with(&format!("{name}="))), "{label}: the control sets {name}");
        }
        // The variations, on a fresh target.
        let case = self.case_for(row).await;
        let before = self.fingerprint(&case.watch).await;
        for variation in route_walk::variations() {
            let Expect::Status(status, code) = (variation.expect)(row.kind) else {
                tally.skipped.entry(variation.name.to_owned()).or_default().push(label.clone());
                continue;
            };
            let mut parts = case.parts.clone();
            (variation.alter)(&mut parts);
            let reply = self.send(&parts).await;
            assert_refused(&label, variation.name, &reply, status, code);
            assert!(unchanged(&before, &self.fingerprint(&case.watch).await), "{label} with {}: the target is unchanged", variation.name);
            tally.ran(variation.name, &label, reply.status);
        }
        // The other methods on the same path.
        let rows = route_walk::table();
        let methods = route_walk::methods_of(&rows, row.path);
        for other in route_walk::OTHER_METHODS.iter().filter(|m| !methods.contains(*m)) {
            let mut parts = case.parts.clone();
            parts.method = (*other).to_owned();
            if *other == "GET" {
                parts.body.clear();
            }
            let reply = self.send(&parts).await;
            assert_refused(&label, &format!("the method {other}"), &reply, 405, "bad_request");
            assert_eq!(self.fingerprint(&case.watch).await, before, "{label} with the method {other}: the target is unchanged");
            tally.ran("the other methods", &label, reply.status);
        }
        // The upload route's own variations.
        if row.kind == Kind::Upload {
            let mut parts = case.parts.clone();
            parts.set("content-type", "application/json");
            let reply = self.send(&parts).await;
            assert_refused(&label, "a JSON content type where the octet stream is required", &reply, 400, "bad_request");
            tally.ran("a JSON content type where the octet stream is required", &label, reply.status);
            // A length over the limit is refused on the header.
            let mut head = case.parts.clone();
            head.remove("content-length");
            let mut request = Request::new(&head.method, &head.target);
            request.headers = head.headers.clone();
            request = request.header("Content-Length", &(256u64 * 1024 * 1024 + 1).to_string());
            let mut conn = self.env.ws.connection().await;
            // Only the head is sent: the refusal is made on the declared length, before any body is read (the server may close
            // the connection at once, so a body write would race with that).
            conn.send(&request.bytes()).await;
            let reply = conn.reply(false).await.expect("an answer on the header");
            self.ledger.note(&reply);
            assert_refused(&label, "a Content-Length over the limit with no body sent", &reply, 400, "bad_request");
            tally.ran("a Content-Length over the limit with no body sent", &label, reply.status);
            assert_eq!(self.fingerprint(&case.watch).await, before, "{label}: nothing was kept");
        }
        // Nothing changed: the same correct call still works, which also shows that the base request of the variations was valid.
        let reply = self.send(&case.parts).await;
        assert_eq!(reply.status, case.success, "{label}: the correct call after the refusals: {} {}", reply.status, reply.text().chars().take(120).collect::<String>());
    }

    /// The sign-in-free bootstrap route, which exists once: the variations run on an environment with no account and a valid
    /// token, then the same token enrols the first administrator, which shows that no refusal spent it.
    async fn run_bootstrap(&self, row: &Row, tally: &mut Tally) {
        let label = row_label(row);
        let enrol = |env: &UnlockEnv| {
            let token = env.accounts.bootstrap_issue().expect("a token").token.expose().to_owned();
            let body = json!({"step": "begin", "token": token, "username": "first", "display_name": "First", "method": "password", "password": "a new horse battery staple"});
            self.page(None, "POST", row.path, Some(&body))
        };
        // The positive control, on its own environment.
        let one = UnlockEnv::start().await;
        let reply = send_on(&one, &enrol(&one)).await;
        assert_eq!((reply.status, reply.json()["next"].as_str()), (200, Some("totp_setup")), "{label}: the control: {}", reply.text());
        // The variations on another one.
        let two = UnlockEnv::start().await;
        let parts = enrol(&two);
        let anonymous = |env: &UnlockEnv| {
            let audit = env.audit_text();
            let accounts = env.ws.test.store.read(|tx| tx.admin_count_active()).expect("the count");
            (audit, accounts)
        };
        let before = anonymous(&two);
        for variation in route_walk::variations() {
            let Expect::Status(status, code) = (variation.expect)(row.kind) else {
                tally.skipped.entry(variation.name.to_owned()).or_default().push(label.clone());
                continue;
            };
            let mut altered = parts.clone();
            (variation.alter)(&mut altered);
            let reply = send_on(&two, &altered).await;
            self.ledger.note(&reply);
            assert_refused(&label, variation.name, &reply, status, code);
            assert_eq!(anonymous(&two), before, "{label} with {}: nothing was created", variation.name);
            tally.ran(variation.name, &label, reply.status);
        }
        let rows = route_walk::table();
        let methods = route_walk::methods_of(&rows, row.path);
        for other in route_walk::OTHER_METHODS.iter().filter(|m| !methods.contains(*m)) {
            let mut altered = parts.clone();
            altered.method = (*other).to_owned();
            if *other == "GET" {
                altered.body.clear();
            }
            let reply = send_on(&two, &altered).await;
            assert_refused(&label, &format!("the method {other}"), &reply, 405, "bad_request");
            tally.ran("the other methods", &label, reply.status);
        }
        let reply = send_on(&two, &parts).await;
        assert_eq!(reply.status, 200, "{label}: the token is still good after every refusal: {}", reply.text());
    }
}

/// Runs a test body on a thread with a big stack: the futures of the rig are large in a debug build.
fn on_big_stack<F: std::future::Future<Output = ()> + Send + 'static>(body: F) {
    let handle = std::thread::Builder::new()
        .name("big-stack".to_owned())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(64 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("a runtime");
            runtime.block_on(body);
        })
        .expect("a thread");
    if let Err(panic) = handle.join() {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn matrix_no_cross_site_or_credential_less_call_changes_anything_and_every_row_has_a_working_control() {
    on_big_stack(matrix_body());
}

async fn matrix_body() {
    let rig = Rig::start().await;
    let rows = route_walk::table();
    let changing: Vec<&Row> = rows.iter().filter(|r| r.changes).collect();
    let mut tally = Tally::default();
    let mut done = Vec::new();
    for row in &changing {
        if row.path == "/auth/bootstrap" {
            rig.run_bootstrap(row, &mut tally).await;
        } else {
            rig.run_row(row, &mut tally).await;
        }
        done.push(row_label(row));
    }
    assert_eq!(done.len(), changing.len(), "every state-changing row ran");
    let problems = rig.header_problems.lock().expect("lock").clone();
    assert!(problems.is_empty(), "headers missing on some answers of the matrix:\n{}", problems.join("\n"));
    println!("Matrix: {} state-changing rows each ran a positive control, {} variations and the correct call again; printed total {} equals the table's changing rows {}", done.len(), tally.ran.len(), done.len(), changing.len());
    for (variation, (ran_on, statuses)) in &tally.ran {
        println!("  {variation}: {} rows, statuses {statuses:?}", ran_on.len());
    }
    for (variation, rows) in &tally.skipped {
        println!("  not applicable ({variation}): {} rows", rows.len());
    }
    println!("Matrix: {} answers checked for headers", rig.answers.load(std::sync::atomic::Ordering::Relaxed));
    let cookie_problems = rig.ledger.problems.lock().expect("lock").clone();
    assert!(cookie_problems.is_empty(), "cookies with a wrong flag:\n{}", cookie_problems.join("\n"));
    for cookie in rig.ledger.cookies.lock().expect("lock").iter() {
        println!("  cookie: {cookie}");
    }
}

/// A little more than the five minutes of the step-up window, in seconds.
const STALE_AFTER: i64 = 360;

/// The members a `403` of the sessions' walk has.
fn forbidden_members(reply: &Reply) -> BTreeSet<String> {
    reply.json().as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

#[test]
fn routes_a_member_is_refused_on_every_administrator_row_and_a_stale_sign_in_is_asked_to_step_up() {
    on_big_stack(async {
        let rig = Rig::start().await;
        let rows = route_walk::table();
        let admin_rows: Vec<&Row> = rows.iter().filter(|r| r.kind == Kind::Admin).collect();
        let me_changes: Vec<&Row> = rows.iter().filter(|r| r.kind == Kind::Fresh && r.changes).collect();
        let body_for = |row: &Row| row.changes.then(|| json!({}));
        let id = rig.bob.id.get().to_string();

        // A member, fresh, on every administrator row: 403 without a hint that a recent sign-in would help.
        for row in &admin_rows {
            let target = route_walk::concrete(row.path, &id);
            let reply = rig.send(&rig.page(Some(&rig.bob_session), row.method, &target, body_for(row).as_ref())).await;
            assert_eq!(reply.status, 403, "a member on {}: {}", row_label(row), reply.text());
            assert_eq!(forbidden_members(&reply), BTreeSet::from(["code".to_owned()]), "{}: no step_up member for a member", row_label(row));
            assert_eq!(body_code(&reply), "forbidden");
        }
        // The control: a fresh administrator is not refused for access reasons.
        let listed = rig.send(&rig.page(Some(&rig.admin_first), "GET", "/api/admin/accounts", None)).await;
        assert_eq!(listed.status, 200, "{}", listed.text());
        let members: BTreeSet<String> = listed.json()["accounts"][0].as_object().expect("an account").keys().cloned().collect();
        assert_eq!(members.len(), 7, "the list has the seven members of the contract: {members:?}");

        // The rows under the gate of the device features answer as absent while both switches are off, so a switch is turned on
        // (with the fresh sign-in the administrator has now) for the rows of the person's grant to be asked for a recent sign-in.
        let on = rig
            .send(&rig.page(Some(&rig.admin_first), "POST", "/api/admin/device-access", Some(&json!({"remote": true}))))
            .await;
        assert_eq!(on.status, 200, "{}", on.text());

        // Six minutes later every sign-in is older than the step-up time.
        rig.env.advance(STALE_AFTER);
        let mut asked = 0;
        for row in admin_rows.iter().chain(me_changes.iter()).filter(|r| r.changes) {
            let target = route_walk::concrete(row.path, &id);
            let reply = rig.send(&rig.page(Some(&rig.admin_first), row.method, &target, body_for(row).as_ref())).await;
            assert_eq!(reply.status, 403, "a stale administrator on {}: {}", row_label(row), reply.text());
            assert_eq!(reply.json(), json!({"code": "forbidden", "step_up": true}), "{}", row_label(row));
            asked += 1;
        }
        // A read still works with an old sign-in.
        for path in ["/api/admin/accounts", "/api/me"] {
            let reply = rig.send(&rig.page(Some(&rig.admin_first), "GET", path, None)).await;
            assert_eq!(reply.status, 200, "{path} with an old sign-in: {}", reply.text());
        }
        // After a step-up the same kind of call is allowed.
        let stepped = rig.send(&rig.page(Some(&rig.admin_first), "POST", "/auth/step-up", Some(&json!({"password": rig.admin.password})))).await;
        assert_eq!(stepped.status, 200, "{}", stepped.text());
        let allowed = rig.send(&rig.page(Some(&rig.admin_first), "POST", &format!("/api/admin/accounts/{id}/unlock-signin"), Some(&json!({})))).await;
        assert_eq!(allowed.status, 204, "after the step-up: {}", allowed.text());
        assert!(rig.header_problems.lock().expect("lock").is_empty(), "{:?}", rig.header_problems.lock().expect("lock"));
        println!(
            "Routes (sessions): a member on {} administrator rows 403 without step_up; a stale sign-in on {asked} changing rows 403 with step_up; a fresh administrator allowed (list 200 with 7 members, unlock-signin 204 after a step-up)",
            admin_rows.len()
        );
    });
}

// =============================================================================================================================
// The administrator sweep: what an administrator can read of another person, on the real binary
// =============================================================================================================================

use std::time::Duration;

use holzwave_host::commands::registry::{COMMANDS, Web};
use sweep::{Canary, Category, Counts};

/// A person of the sweep: a client with a recorder of its own, so what each of them received is kept apart.
struct Person {
    client: Client,
    recorder: std::sync::Arc<real_process::Recorder>,
    password: String,
}

fn person(server: &real_process::Server, name: &'static str) -> Person {
    let recorder = std::sync::Arc::new(real_process::Recorder::default());
    let client = Client::new(server.site.addr(), server.site.der.clone(), recorder.clone());
    Person { client, recorder, password: format!("{name} correct horse battery staple") }
}

/// The commands the sweep sends from the administrator's socket: every command the browser has that answers something (the
/// answer type is not the unit type). Read from the table at run time.
fn data_commands() -> Vec<&'static holzwave_host::commands::registry::Command> {
    COMMANDS.iter().filter(|c| c.web != Web::Unavailable && c.answer.replace(' ', "") != "()").collect()
}

/// Where a canary was found, as text for a failure (never the canary).
fn finding_lines(found: &[sweep::Finding]) -> Vec<String> {
    found.iter().map(sweep::Finding::line).collect()
}

fn scan_person(sweep: &sweep::Sweep, surface_prefix: &str, recorder: &real_process::Recorder, counts: &mut Counts) -> Vec<sweep::Finding> {
    let mut found = Vec::new();
    for (index, exchange) in recorder.exchanges().iter().enumerate() {
        counts.bytes += exchange.body.len();
        let name = format!("{surface_prefix} response #{index} {} {}", exchange.method, exchange.target.split('?').next().unwrap_or_default());
        found.extend(sweep.scan_bytes(&format!("{name} body"), &exchange.body));
        let headers: String = exchange.headers.iter().map(|(n, v)| format!("{n}: {v}\n")).collect();
        found.extend(sweep.scan_text(&format!("{name} headers"), &headers));
        found.extend(sweep.scan_text(&format!("{name} target"), &exchange.target));
    }
    for (index, frame) in recorder.frames().iter().enumerate() {
        counts.lines += 1;
        found.extend(sweep.scan_text(&format!("{surface_prefix} frame #{index}"), frame));
    }
    found
}

/// One command at a rate below the socket's limit; an answer that says "too many" is counted, never accepted silently.
async fn paced(socket: &mut real_process::Socket, name: &str, args: &Value, throttled: &mut usize) -> Value {
    tokio::time::sleep(Duration::from_millis(4)).await;
    let answer = socket.call(name, args).await;
    if answer.to_string().contains("Too many commands") {
        *throttled += 1;
    }
    answer
}

/// The arguments for a command with one argument set to `value`.
fn args_with(row: &holzwave_host::commands::registry::Command, name: &str, value: Value) -> Value {
    let mut args = parity_env::args_for(row);
    args.as_object_mut().expect("an object").insert(name.to_owned(), value);
    args
}

#[test]
fn admin_sweep_an_administrator_reads_nothing_of_another_person_through_any_route_or_command() {
    on_big_stack(admin_sweep_body());
}

async fn admin_sweep_body() {
    let owner = "bob".to_owned();
    let print = Canary::new("content printed", Category::Content { owner: owner.clone() });
    let saved = Canary::new("content saved", Category::Content { owner: owner.clone() });
    let uploaded = Canary::new("content uploaded", Category::Content { owner: owner.clone() });
    let key = Canary::new("assistant key", Category::Secret);
    let b_password = Canary::new("bob password", Category::Secret);

    let server = Site::new(None).start();
    let setup = server.setup_token();
    let mut alice = person(&server, "alice");
    let mut bob = person(&server, "bob");
    let mut carol = person(&server, "carol");
    let alice_enrolled = alice.client.enrol("/auth/bootstrap", &setup, "alice", &alice.password).await;
    let invitation_b = alice.client.invitation("member").await;
    bob.password = b_password.text().to_owned();
    let bob_enrolled = bob.client.enrol("/auth/invite/redeem", &invitation_b, "bob", &bob.password).await;
    let invitation_c = alice.client.invitation("member").await;
    carol.client.enrol("/auth/invite/redeem", &invitation_c, "carol", &carol.password).await;
    let b_totp = Canary::with_text("bob authenticator secret", Category::Secret, &bob_enrolled.secret);
    let b_recovery = Canary::with_text("bob recovery code", Category::Secret, &bob_enrolled.recovery);

    // ---- B uses the product: a shell with printed and typed text, a saved file, a secret, an upload and a download link ----
    let mut socket_b = bob.client.socket().await;
    let pane_b = socket_b.start_focused_pane().await;
    let (first, second) = print.text().split_at(14);
    socket_b.type_line(pane_b, &format!("A={first}; B={second}; printf '%s\\n' \"$A$B\"")).await;
    assert!(
        socket_b.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains(print.text())).await.is_some(),
        "bob's shell printed his canary"
    );
    let mut put = bob.client.page_request("PUT", "/api/upload?name=canary.png", None).header("Content-Type", "application/octet-stream");
    put.body = [&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..], uploaded.text().as_bytes()].concat();
    let answer = bob.client.send(put).await;
    assert_eq!(answer.status, 200, "upload: {}", answer.text());
    let b_path = answer.json()["path"].as_str().expect("the path of the upload").to_owned();
    let b_dir = Path::new(&b_path).parent().expect("a directory").to_path_buf();
    let saved_path = b_dir.join("saved.txt").to_string_lossy().into_owned();
    let write = socket_b.call("file_save", &json!({"source": "local", "path": saved_path, "text": saved.text()})).await;
    let file_saved = write.get("ok").is_some();
    let profile = socket_b.ok("ai_profile_save", &json!({"input": {"name": "Cloud", "preset": "anthropic"}})).await.as_str().expect("an id").to_owned();
    let stored = socket_b.call("ai_key_set", &json!({"profile": profile, "slot": "provider", "api_key": key.text()})).await;
    let key_stored = stored.get("ok").is_some();
    let grant = socket_b.ok("file_grant", &json!({"source": "local", "path": b_path})).await;
    let url = grant["url"].as_str().expect("a download address").to_owned();
    let token_b = url.rsplit('/').next().unwrap_or_default().split('?').next().unwrap_or_default().to_owned();
    let own_download = bob.client.call("GET", &url, None).await;
    assert_eq!(own_download.status, 200, "bob reads his own download");
    println!("Admin sweep (setup): file saved {file_saved}, assistant key stored {key_stored}, link token of {} characters", token_b.len());

    let all = [print.clone(), saved.clone(), uploaded.clone(), key.clone(), b_password.clone(), b_totp, b_recovery];
    let sweep = sweep::Sweep::new(&all);

    // ---- the positive control for the sweep: the sweep over bob's own recorder sees bob's content ----
    let mut control_counts = Counts::default();
    let own_view = scan_person(&sweep, "bob", &bob.recorder, &mut control_counts);
    assert!(own_view.iter().any(|f| f.canary == "content printed" && f.surface.contains("frame #")), "the sweep sees bob's printed text in bob's frames");
    assert!(own_view.iter().any(|f| f.canary == "content uploaded" && f.surface.contains("GET /files/")), "the sweep sees bob's upload in bob's download");

    // ---- A signs in freshly and calls every administrator route ----
    let stepped = alice.client.call("POST", "/auth/step-up", Some(&json!({"password": alice.password}))).await;
    assert_eq!(stepped.status, 200, "{}", stepped.text());
    let rows = route_walk::table();
    let listed = alice.client.call("GET", "/api/admin/accounts", None).await;
    assert_eq!(listed.status, 200, "{}", listed.text());
    let accounts = listed.json()["accounts"].as_array().expect("accounts").clone();
    let member_names: BTreeSet<String> = accounts[0].as_object().expect("an account").keys().cloned().collect();
    assert_eq!(member_names.len(), 7, "the list holds the seven members of the contract: {member_names:?}");
    let id_of = |name: &str| -> String {
        accounts.iter().find(|a| a["username"] == name).unwrap_or_else(|| panic!("{name} is listed"))["id"].to_string().trim_matches('"').to_owned()
    };
    let (id_b, id_c) = (id_of("bob"), id_of("carol"));
    let mut routes_called = 0;
    let mut statuses = BTreeSet::new();
    for row in rows.iter().filter(|r| r.kind == Kind::Admin) {
        let methods = route_walk::methods_of(&rows, row.path);
        for method in methods.iter() {
            // Changing calls go to carol, never to bob; the listing and the invitation have no target.
            let target = route_walk::concrete(row.path, &id_c);
            if *method == "DELETE" {
                continue; // carol is deleted last, below
            }
            // A path with a read and a change (the switches) is visited once per row: the body follows the method.
            let body = (*method != "GET").then(|| match row.path {
                p if p.ends_with("/role") => json!({"role": "member"}),
                "/api/admin/invitations" => json!({"role": "member", "days": 1}),
                // Turning a switch off that is already off changes nothing: the call is made, and no state moves.
                "/api/admin/device-access" => json!({"sync": false}),
                _ => json!({}),
            });
            let reply = alice.client.call(method, &target, body.as_ref()).await;
            statuses.insert(reply.status);
            assert!(matches!(reply.status, 200 | 204), "{method} {target}: {} {}", reply.status, reply.text());
            routes_called += 1;
        }
    }
    // A also tries bob's id on the read-like shapes it can: the list is the only read; everything else would change bob and is
    // not sent. A bob-shaped id that is not bob's (a neighbour) is refused or absent.
    for id in ["0", "999999", "-1", "1e3"] {
        for suffix in ["", "/role", "/disable"] {
            let target = format!("/api/admin/accounts/{id}{suffix}");
            let method = if suffix.is_empty() { "DELETE" } else { "POST" };
            let body = (!suffix.is_empty()).then(|| if suffix == "/role" { json!({"role": "member"}) } else { json!({}) });
            let reply = alice.client.call(method, &target, body.as_ref()).await;
            assert!(reply.status >= 400, "{method} {target}: an unknown id is refused, got {}", reply.status);
        }
    }
    let after = alice.client.call("GET", "/api/admin/accounts", None).await;
    assert_eq!(after.status, 200);
    let still: BTreeSet<String> = after.json()["accounts"].as_array().expect("accounts").iter().map(|a| a["username"].as_str().unwrap_or_default().to_owned()).collect();
    assert!(still.contains("bob") && id_b != id_c, "bob was not touched");
    println!("Admin sweep (routes): {routes_called} administrator calls with every listed method but the deletion, statuses {statuses:?}; list members {member_names:?}");

    // ---- A's own socket: every data command, with bob's ids, paths and token ----
    let mut socket_a = alice.client.socket().await;
    let pane_a = socket_a.start_focused_pane().await;
    let mut throttled = 0usize;
    let mut commands_sent = 0usize;
    let mut calls = 0usize;
    let mut oks_with_b_pane = 0usize;
    let paths = [b_path.clone(), format!("{}/../{}", b_dir.display(), Path::new(&b_path).file_name().and_then(|n| n.to_str()).unwrap_or("")), Path::new(&b_path).file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned(), saved_path.clone()];
    for row in data_commands() {
        let names: Vec<&str> = row.args.iter().map(|a| a.name).collect();
        let mut sent = false;
        if names.contains(&"pane") {
            for id in 1..=64u64 {
                // Closing her own last pane ends her own app; with her own id that is a call on her own layout, not on bob's.
                if id == pane_a && row.name.starts_with("close_") {
                    continue;
                }
                let answer = paced(&mut socket_a, row.name, &args_with(row, "pane", json!(id)), &mut throttled).await;
                calls += 1;
                sent = true;
                if id == pane_b && pane_b != pane_a && answer.get("ok").is_some() {
                    oks_with_b_pane += 1;
                }
            }
        }
        if names.contains(&"path") {
            for path in &paths {
                paced(&mut socket_a, row.name, &args_with(row, "path", json!(path)), &mut throttled).await;
                calls += 1;
                sent = true;
            }
        }
        if names.contains(&"token") {
            paced(&mut socket_a, row.name, &args_with(row, "token", json!(token_b)), &mut throttled).await;
            calls += 1;
            sent = true;
        }
        if !sent {
            paced(&mut socket_a, row.name, &parity_env::args_for(row), &mut throttled).await;
            calls += 1;
            sent = true;
        }
        commands_sent += usize::from(sent);
    }
    let layout_a = socket_a.ok("get_layout", &json!({})).await;
    assert_eq!(throttled, 0, "the socket's rate limit did not end the sweep early");
    println!("Admin sweep (commands): {commands_sent} data commands of {} in the table, {calls} calls, pane ids 1 to 64, {} paths, bob's token; alice's pane {pane_a}, bob's pane {pane_b}", COMMANDS.len(), paths.len());

    // A file route with A's session and bob's token.
    let mut route_statuses = BTreeSet::new();
    for target in [url.clone(), format!("{url}?download=1"), format!("/files/{token_b}")] {
        let reply = alice.client.call("GET", &target, None).await;
        route_statuses.insert(reply.status);
        assert!(matches!(reply.status, 403 | 404), "a download link of bob's with alice's session: {} for {target}", reply.status);
    }
    let sneaky = alice.client.call("GET", &format!("/files/{token_b}/../../etc/passwd"), None).await;
    assert!(sneaky.status >= 400, "a path under the link is refused");
    println!("Admin sweep (file route): bob's link with alice's session answers {route_statuses:?}");

    // ---- the sweep over everything A received or caused ----
    alice.client.call("DELETE", &format!("/api/admin/accounts/{id_c}"), None).await;
    let mut counts = Counts::default();
    let mut found = scan_person(&sweep, "alice", &alice.recorder, &mut counts);
    found.extend(sweep.scan_lines("stdout", &server.stdout_lines(), &mut counts));
    found.extend(sweep.scan_lines("stderr", &server.stderr_lines(), &mut counts));
    let audit_lines: Vec<String> = server.audit().iter().map(Value::to_string).collect();
    found.extend(sweep.scan_lines("audit", &audit_lines, &mut counts));
    assert!(found.is_empty(), "something of bob's reached alice or the logs: {:?}", finding_lines(&found));
    println!(
        "Admin sweep (surfaces): {} responses of alice, {} frames of alice, {} audit lines, {} lines of standard output and error; hits: none",
        alice.recorder.exchanges().len(),
        alice.recorder.frames().len(),
        audit_lines.len(),
        server.stdout_lines().len() + server.stderr_lines().len()
    );

    // alice's layout is alice's: her panes and nothing of bob's text.
    let layout_text = layout_a.to_string();
    assert!(sweep.scan_text("alice layout", &layout_text).is_empty(), "alice's layout holds nothing of bob's");
    assert_eq!(oks_with_b_pane, oks_with_b_pane.min(calls), "counted");
    let _ = (&alice_enrolled, &carol);

    // ---- the positive control: bob's canary placed in alice's own pane is found in alice's frames ----
    // The barrage may have closed or stopped her pane, so the control uses a fresh socket and its own focused pane.
    drop(socket_a);
    let mut control_socket = alice.client.socket().await;
    let control_pane = control_socket.start_focused_pane().await;
    let (first, second) = print.text().split_at(14);
    control_socket.type_line(control_pane, &format!("A={first}; B={second}; printf '%s\\n' \"$A$B\"")).await;
    assert!(control_socket.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains(print.text())).await.is_some(), "alice's shell printed it");
    let planted = scan_person(&sweep, "alice", &alice.recorder, &mut Counts::default());
    assert!(planted.iter().any(|f| f.canary == "content printed" && f.surface.contains("frame #")), "the sweep over alice's recorder finds a canary that is in her own pane");
    println!("Admin sweep (positive control): the canary printed in alice's own pane is found in {} of her frames", planted.iter().filter(|f| f.canary == "content printed").count());
}

// =============================================================================================================================
// The start matrix: a mistaken configuration never runs
// =============================================================================================================================

/// One mistake of the start: how to make it, the word the sentence must contain, and the same builder with the mistake mended.
struct Mistake {
    name: &'static str,
    /// The text the one error line must contain: the file or the key at fault.
    names: &'static str,
    /// Whether the key file variable is set.
    with_key: bool,
    bad: fn(&mut Site),
    /// The same builder with the value made right: it must start.
    good: fn(&mut Site),
}

fn keep(_: &mut Site) {}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

fn mistakes() -> Vec<Mistake> {
    let m = |name, names, with_key, bad, good| Mistake { name, names, with_key, bad, good };
    vec![
        m("the key file variable is not set", "HOLZWAVE_SERVER_KEY_FILE", false, keep, keep),
        m("the key file is missing", "key", true, |s| s.key = s.scratch.join("no-such-key"), keep),
        m("the key file is readable by others (0644)", "key", true, |s| chmod(&s.key.clone(), 0o644), |s| chmod(&s.key.clone(), 0o600)),
        m("the key file is all zero", "key", true, |s| {
            let len = std::fs::metadata(&s.key).expect("key").len() as usize;
            std::fs::write(&s.key, vec![0u8; len]).expect("write");
            chmod(&s.key.clone(), 0o600);
        }, keep),
        m("the key file is not a key", "key", true, |s| {
            std::fs::write(&s.key, b"not a key file\n").expect("write");
            chmod(&s.key.clone(), 0o600);
        }, keep),
        m("public_url is missing", "public_url", true, |s| s.unset("public_url"), keep),
        m("public_url is empty", "public_url", true, |s| s.set("public_url", json!("")), keep),
        m("public_url is http", "public_url", true, |s| s.set("public_url", json!("http://hw.example.test")), keep),
        m("public_url has a path", "public_url", true, |s| s.set("public_url", json!("https://hw.example.test/app")), keep),
        m("public_url has a query", "public_url", true, |s| s.set("public_url", json!("https://hw.example.test/?a=1")), keep),
        m("public_url has a fragment", "public_url", true, |s| s.set("public_url", json!("https://hw.example.test/#x")), keep),
        m("public_url has user information", "public_url", true, |s| s.set("public_url", json!("https://me@hw.example.test")), keep),
        m("public_url is an address and passkeys are on", "public_url", true, |s| s.set("public_url", json!("https://192.0.2.7")), keep),
        m("the tls section is missing", "tls", true, |s| s.unset("tls"), keep),
        m("the certificate file is missing", "cert_file", true, |s| s.set("tls.cert_file", json!("/no/such/cert.pem")), keep),
        m("the certificate key file is missing", "key_file", true, |s| s.set("tls.key_file", json!("/no/such/key.pem")), keep),
        m("the certificate file is unreadable (0000)", "cert_file", true, |s| {
            let path = PathBuf::from(s.config["tls"]["cert_file"].as_str().expect("cert"));
            chmod(&path, 0o000);
        }, |s| {
            let path = PathBuf::from(s.config["tls"]["cert_file"].as_str().expect("cert"));
            chmod(&path, 0o644);
        }),
        m("a plain listener with no trusted proxy", "trusted_proxies", true, |s| s.set("tls", json!({"mode": "proxy"})), |s| {
            s.set("tls", json!({"mode": "proxy", "trusted_proxies": ["10.0.0.0/8"]}));
        }),
        m("a plain listener with an empty proxy list", "trusted_proxies", true, |s| s.set("tls", json!({"mode": "proxy", "trusted_proxies": []})), |s| {
            s.set("tls", json!({"mode": "proxy", "trusted_proxies": ["fd00::/8"]}));
        }),
        m("a plain listener that trusts every address", "trusted_proxies", true, |s| s.set("tls", json!({"mode": "proxy", "trusted_proxies": ["0.0.0.0/0"]})), |s| {
            s.set("tls", json!({"mode": "proxy", "trusted_proxies": ["172.16.0.0/12"]}));
        }),
        m("no sign-in method", "sign-in", true, |s| s.set("auth", json!({"passkey": false, "password_totp": false})), keep),
        m("no auth section", "sign-in", true, |s| s.unset("auth"), keep),
        m("an unknown key at the top", "extra", true, |s| s.set("extra", json!(1)), keep),
        m("an unknown key in tls", "extra", true, |s| s.set("tls.extra", json!(1)), keep),
        m("an unknown key in workers", "extra", true, |s| s.set("workers.extra", json!(1)), keep),
        m("an unknown key in auth", "extra", true, |s| s.set("auth.extra", json!(true)), keep),
        m("the worker range starts below 1000", "uid_min", true, |s| s.set("workers.uid_min", json!(0)), keep),
        m("the worker range is reversed", "uid_min", true, |s| {
            s.set("workers.uid_min", json!(29999));
            s.set("workers.uid_max", json!(20000));
        }, keep),
        m("the worker range ends at the largest id", "uid_max", true, |s| s.set("workers.uid_max", json!(4294967295u64)), keep),
        m("listen is not an address", "listen", true, |s| s.set("listen", json!("everywhere")), |s| {
            let port = s.port;
            s.set("listen", json!(format!("127.0.0.1:{port}")));
        }),
        m("the data directory is relative", "data_dir", true, |s| s.set("data_dir", json!("data")), keep),
        m("the runtime directory is relative", "runtime_dir", true, |s| s.set("runtime_dir", json!("run")), keep),
        m("a sign-in provider without a client id", "client_id", true, |s| {
            s.set("auth.oidc", json!([{"id": "corp", "name": "Company", "issuer": "https://idp.example.test", "client_id": "", "client_secret_file": "/no/such/secret"}]));
        }, keep),
        m("a sign-in provider with an http issuer", "issuer", true, |s| {
            s.set("auth.oidc", json!([{"id": "corp", "name": "Company", "issuer": "http://idp.example.test", "client_id": "x", "client_secret_file": "/no/such/secret"}]));
        }, keep),
    ]
}

#[test]
fn start_matrix_a_mistaken_configuration_exits_with_one_sentence_and_leaves_nothing_behind() {
    let rows = mistakes();
    let mut printed = Vec::new();
    let root = rustix::process::geteuid().is_root();
    for row in &rows {
        if root && row.name.contains("unreadable") {
            // A process that runs as root reads every file, so this mistake cannot be made by a mode.
            println!("Start matrix: NOT RUN as root: {}", row.name);
            continue;
        }
        // The positive control of the builder: the same site with the mistake mended starts and is healthy.
        let mut good = Site::new(None);
        (row.good)(&mut good);
        let key_text = std::fs::read(&good.key).unwrap_or_default();
        let mut started = good.start();
        assert!(started.is_running(), "{}: the control starts", row.name);
        drop(started);

        let mut site = Site::new(None);
        let original_key = std::fs::read(&site.key).expect("the key");
        let cert_key = site.config["tls"]["key_file"].as_str().map(|p| std::fs::read(p).unwrap_or_default()).unwrap_or_default();
        (row.bad)(&mut site);
        let refusal = site.refuse(row.with_key, Duration::from_secs(5));
        let lines: Vec<&String> = refusal.stderr.iter().filter(|l| !l.trim().is_empty()).collect();
        assert!(!refusal.still_running, "{}: the process ended on its own", row.name);
        assert_eq!(refusal.code, Some(1), "{}: exit code", row.name);
        assert!(refusal.elapsed < Duration::from_secs(5), "{}: ended within 5 seconds, took {:?}", row.name, refusal.elapsed);
        assert_eq!(lines.len(), 1, "{}: exactly one line on standard error, got {:?}", row.name, refusal.stderr);
        assert!(lines[0].contains(row.names), "{}: the sentence names {:?}: {}", row.name, row.names, lines[0]);
        assert!(refusal.stdout.is_empty(), "{}: nothing on standard output: {:?}", row.name, refusal.stdout);
        assert!(!refusal.listening, "{}: nothing listens on the configured port", row.name);
        assert!(refusal.new_files.is_empty(), "{}: no new file: {:?}", row.name, refusal.new_files);
        for secret in [&original_key, &cert_key, &key_text] {
            let text = String::from_utf8_lossy(secret);
            let trimmed = text.trim();
            assert!(trimmed.len() < 8 || !lines[0].contains(trimmed), "{}: the sentence holds file content", row.name);
        }
        assert!(!lines[0].contains("PRIVATE KEY"), "{}: no key material in the sentence", row.name);
        printed.push(format!("{}: exit 1 in {:?}, one line, stdout empty, nothing listening, no new file; control starts", row.name, refusal.elapsed));
    }
    println!("Start matrix: {} mistakes, each with the builder's positive control", rows.len());
    for line in &printed {
        println!("  {line}");
    }
    println!("Start matrix: workers.mode is covered by the image test where the release binary refuses the in-process mode; this test binary is built with the feature that allows it");
}

// =============================================================================================================================
// No default account, no plain listener
// =============================================================================================================================

/// The names a default account might have, and the passwords such an account might have.
const DEFAULT_NAMES: [&str; 6] = ["admin", "administrator", "root", "holzwave", "owner", "test"];
const DEFAULT_PASSWORDS: [&str; 6] = ["admin", "password", "changeme", "holzwave", "admin123", "letmein"];

/// The pairs to try: round robin over the names, never more than the throttle's limit for one address less a margin, and never
/// more tries for one name than its own limit less one. Both limits are read from the limits module.
fn default_pairs() -> Vec<(&'static str, &'static str)> {
    let limits = holzwave_server::limits::Limits::DEFAULT;
    let per_address = limits.password_attempts_per_address as usize - 4;
    let per_account = limits.password_attempts_per_account as usize - 1;
    let mut pairs = Vec::new();
    let mut tries: BTreeMap<&str, usize> = BTreeMap::new();
    'outer: for round in 0..DEFAULT_PASSWORDS.len() {
        for (index, name) in DEFAULT_NAMES.iter().enumerate() {
            if pairs.len() >= per_address {
                break 'outer;
            }
            if *tries.get(name).unwrap_or(&0) >= per_account {
                continue;
            }
            *tries.entry(name).or_default() += 1;
            pairs.push((*name, DEFAULT_PASSWORDS[(round + index) % DEFAULT_PASSWORDS.len()]));
        }
    }
    pairs
}

#[test]
fn negative_control_default_pairs_stay_below_both_limits_and_cover_every_name() {
    let pairs = default_pairs();
    let limits = holzwave_server::limits::Limits::DEFAULT;
    assert!(pairs.len() < limits.password_attempts_per_address as usize);
    for name in DEFAULT_NAMES {
        let tries = pairs.iter().filter(|(n, _)| *n == name).count();
        assert!(tries >= 1 && tries < limits.password_attempts_per_account as usize, "{name}: {tries} tries");
    }
    assert_eq!(pairs.iter().collect::<BTreeSet<_>>().len(), pairs.len(), "no pair twice");
}

/// What a TCP connection to the TLS port that speaks plain HTTP reads back within 3 seconds.
fn plain_tcp(addr: std::net::SocketAddr, send: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(3))).expect("timeout");
    let _ = stream.write_all(send);
    let mut got = Vec::new();
    let mut buffer = [0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => got.extend_from_slice(&buffer[..n]),
        }
    }
    got
}

#[test]
fn default_admin_no_default_account_signs_in_and_the_tls_port_never_answers_plain_http() {
    on_big_stack(async {
        let server = Site::new(None).start();
        let mut client = server.client();
        let before = client.session().await;
        assert_eq!(before["bootstrap"], json!(true), "a valid start with no account offers the setup");
        let list = client.call("GET", "/api/admin/accounts", None).await;
        assert_eq!(list.status, 401, "the account list needs a session: {}", list.text());

        let pairs = default_pairs();
        let mut sentences = BTreeSet::new();
        let mut statuses = BTreeSet::new();
        for (name, password) in &pairs {
            let reply = client.call("POST", "/auth/password", Some(&json!({"username": name, "password": password}))).await;
            statuses.insert(reply.status);
            sentences.insert(reply.text());
            assert!(reply.all("set-cookie").iter().all(|c| !c.starts_with("__Host-hw_session=") || c.contains("Max-Age=0")), "{name}: no session");
        }
        assert_eq!(statuses, BTreeSet::from([401]), "every pair is refused");
        assert_eq!(sentences.len(), 1, "every pair gets the one sentence: {sentences:?}");
        let after = client.session().await;
        assert_eq!(after["bootstrap"], json!(true), "the attempts did not end the setup");
        println!(
            "Default admin: {} pairs ({} names x {} passwords, capped below the per-address limit {} and the per-account limit {}); status {statuses:?}; one sentence {:?}; bootstrap still true; account list without a session 401",
            pairs.len(),
            DEFAULT_NAMES.len(),
            DEFAULT_PASSWORDS.len(),
            holzwave_server::limits::Limits::DEFAULT.password_attempts_per_address,
            holzwave_server::limits::Limits::DEFAULT.password_attempts_per_account,
            sentences.iter().next().map(String::as_str).unwrap_or_default()
        );
        println!("Default admin pairs: {}", pairs.iter().map(|(n, p)| format!("{n}/{p}")).collect::<Vec<_>>().join(" "));

        // Plain bytes on the TLS port.
        let addr = server.site.addr();
        for (what, bytes) in [("GET", &b"GET / HTTP/1.1\r\nHost: hw.example.test\r\n\r\n"[..]), ("CONNECT", &b"CONNECT hw.example.test:443 HTTP/1.1\r\nHost: hw.example.test:443\r\n\r\n"[..])] {
            let got = plain_tcp(addr, bytes);
            assert!(!got.starts_with(b"HTTP/") && !String::from_utf8_lossy(&got).contains("HTTP/1"), "{what}: the TLS port answers plain HTTP: {got:?}");
            println!("Plain TCP: {what} sent as plain bytes to the TLS port; read {} bytes within 3 seconds, first bytes {:02x?}", got.len(), &got[..got.len().min(7)]);
        }
        // The control: the same port, with TLS, answers HTTP.
        assert_eq!(client.session().await["bootstrap"], json!(true), "the port answers through TLS");
    });
}

/// The sockets the process tree listens on, as text, on Linux; read from `/proc` and matched to the descriptors by inode.
#[cfg(target_os = "linux")]
fn listening_sockets(root: u32) -> Vec<String> {
    fn children(of: u32) -> Vec<u32> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
            let after = stat.rsplit(')').next().unwrap_or_default();
            if after.split_whitespace().nth(1).and_then(|p| p.parse::<u32>().ok()) == Some(of) {
                found.push(pid);
            }
        }
        found
    }
    let mut tree = vec![root];
    let mut at = 0;
    while at < tree.len() {
        let more = children(tree[at]);
        tree.extend(more);
        at += 1;
    }
    let mut inodes = BTreeSet::new();
    for pid in &tree {
        for fd in std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let text = target.to_string_lossy().into_owned();
                if let Some(inode) = text.strip_prefix("socket:[").and_then(|t| t.strip_suffix(']')) {
                    inodes.insert(inode.to_owned());
                }
            }
        }
    }
    let mut found = Vec::new();
    for pid in &tree {
        for table in ["tcp", "tcp6"] {
            for line in std::fs::read_to_string(format!("/proc/{pid}/net/{table}")).unwrap_or_default().lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() > 9 && fields[3] == "0A" && inodes.contains(fields[9]) {
                    found.push(format!("{table} {}", fields[1]));
                }
            }
        }
        for line in std::fs::read_to_string(format!("/proc/{pid}/net/unix")).unwrap_or_default().lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 8 && inodes.contains(fields[6]) && fields[5] == "01" {
                found.push(format!("unix {}", fields[7]));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[test]
fn listeners_the_process_tree_listens_on_the_configured_address_and_the_health_socket_only() {
    #[cfg(target_os = "linux")]
    {
        let server = Site::new(None).start();
        let found = listening_sockets(server.pid());
        let port = server.site.port;
        // The health socket is bound inside a private directory of the runtime directory and then moved to its name; the
        // kernel lists the path it was bound at.
        let run = server.site.run_dir().display().to_string();
        let wanted_tcp = format!("tcp 0100007F:{port:04X}");
        let tcp: Vec<&String> = found.iter().filter(|f| f.starts_with("tcp")).collect();
        let unix: Vec<&String> = found.iter().filter(|f| f.starts_with("unix")).collect();
        assert_eq!(tcp, [&wanted_tcp], "one TCP listener, the configured address: {found:?}");
        assert!(unix.len() == 1 && unix[0].starts_with(&format!("unix {run}/")), "one Unix socket, inside the runtime directory: {found:?}");
        println!("Listeners: {found:?}");
    }
    #[cfg(not(target_os = "linux"))]
    {
        println!("Listeners: NOT RUN: not Linux (the process tree's sockets are read from /proc; the Linux container run covers it)");
    }
}

// =============================================================================================================================
// The deployment files
// =============================================================================================================================

/// The environment variable that the sign-off run sets to allow a run without the deployment tools. Without it a missing tool
/// fails the test.
const TOOLFREE: &str = "HOLZWAVE_SECURITY_TOOLFREE";

/// What is wrong with a Dockerfile text: the rules of the runtime contract that can be read from the text. Comment lines are
/// not instructions and are ignored.
fn dockerfile_problems(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    // Join continuation lines and drop comments.
    let mut joined = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            joined.push_str(stripped);
            joined.push(' ');
        } else {
            joined.push_str(trimmed);
            joined.push('\n');
        }
    }
    let lines: Vec<&str> = joined.lines().filter(|l| !l.trim().is_empty()).collect();
    let froms: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.to_ascii_uppercase().starts_with("FROM ")).map(|(i, _)| i).collect();
    let Some(&last_from) = froms.last() else {
        return vec!["no FROM instruction".to_owned()];
    };
    for &at in &froms {
        let image = lines[at].split_whitespace().nth(1).unwrap_or_default();
        let reference = image.split('@').next().unwrap_or_default();
        if reference.ends_with(":latest") || (!reference.contains(':') && !image.contains('@') && reference != "scratch") {
            problems.push(format!("the image {image} has no pinned tag (latest)"));
        }
    }
    let final_stage = &lines[last_from..];
    match final_stage.iter().rev().find(|l| l.to_ascii_uppercase().starts_with("USER ")) {
        None => problems.push("the final stage has no USER instruction".to_owned()),
        Some(user) => {
            let name = user.split_whitespace().nth(1).unwrap_or_default().split(':').next().unwrap_or_default();
            if name == "root" || name == "0" {
                problems.push("the final stage runs as root".to_owned());
            }
        }
    }
    if !final_stage.iter().any(|l| l.to_ascii_uppercase().starts_with("HEALTHCHECK ") && !l.to_ascii_uppercase().contains("NONE")) {
        problems.push("the final stage has no HEALTHCHECK".to_owned());
    }
    let setcaps: Vec<&&str> = lines.iter().filter(|l| l.split(|c: char| !c.is_alphanumeric() && c != '-').any(|w| w == "setcap")).collect();
    if setcaps.len() != 1 {
        problems.push(format!("{} setcap instructions where exactly one is allowed", setcaps.len()));
    }
    for line in &setcaps {
        let after = line.split("setcap").nth(1).unwrap_or_default();
        let spec = after.split_whitespace().find(|w| w.contains("cap_")).unwrap_or_default();
        let (caps, flags) = spec.split_once('=').unwrap_or((spec, ""));
        let set: BTreeSet<&str> = caps.split(',').collect();
        let wanted: BTreeSet<&str> = ["cap_setuid", "cap_setgid", "cap_chown", "cap_kill"].into_iter().collect();
        if set != wanted || flags != "p" {
            problems.push(format!("the file capabilities are {caps}={flags}, not the four of the contract with =p"));
        }
    }
    for forbidden in ["--privileged", "no-new-privileges", "docker.sock", "/var/run/docker", "containerd.sock"] {
        if lines.iter().any(|l| l.contains(forbidden)) {
            problems.push(format!("the text names {forbidden}"));
        }
    }
    problems
}

#[test]
fn deploy_the_dockerfile_keeps_the_runtime_contract() {
    let text = read(&repo_root().join("packaging/docker/Dockerfile"));
    let problems = dockerfile_problems(&text);
    assert!(problems.is_empty(), "the Dockerfile: {problems:?}");
    println!("Deploy (Dockerfile): final stage USER 10001:10001, a HEALTHCHECK, pinned images, one setcap with the four capabilities =p, no privileged, socket or no-new-privileges instruction");
}

#[test]
fn negative_control_deploy_each_rule_of_the_dockerfile_guard_reports_its_own_mistake() {
    let good = read(&repo_root().join("packaging/docker/Dockerfile"));
    assert!(dockerfile_problems(&good).is_empty(), "the real file passes");
    let mutations: [(&str, Box<dyn Fn(&str) -> String>, &str); 9] = [
        ("root", Box::new(|t| t.replace("USER 10001:10001", "USER root")), "root"),
        ("no user", Box::new(|t| t.replace("USER 10001:10001", "")), "no USER"),
        ("no healthcheck", Box::new(|t| t.lines().filter(|l| !l.starts_with("HEALTHCHECK")).collect::<Vec<_>>().join("\n")), "HEALTHCHECK"),
        ("latest", Box::new(|t| t.replacen("debian:trixie-20261027-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4", "debian:latest", 1)), "latest"),
        ("extra capability", Box::new(|t| t.replace("cap_setuid,cap_setgid,cap_chown,cap_kill=p", "cap_setuid,cap_setgid,cap_chown,cap_kill,cap_sys_admin=p")), "file capabilities"),
        ("a second setcap", Box::new(|t| format!("{t}\nRUN setcap cap_chown=p /usr/bin/x\n")), "setcap"),
        ("privileged", Box::new(|t| format!("{t}\nRUN docker run --privileged x\n")), "--privileged"),
        ("no-new-privileges", Box::new(|t| format!("{t}\nRUN echo --security-opt no-new-privileges\n")), "no-new-privileges"),
        ("socket", Box::new(|t| format!("{t}\nVOLUME /var/run/docker.sock\n")), "docker"),
    ];
    for (name, mutate, needle) in mutations {
        let problems = dockerfile_problems(&mutate(&good));
        assert!(problems.iter().any(|p| p.contains(needle)), "{name}: reported {problems:?}, wanted a problem naming {needle:?}");
    }
    // A comment that names a forbidden option is not an instruction.
    assert!(dockerfile_problems(&format!("{good}\n# Do not add no-new-privileges or --privileged here\n")).is_empty(), "comments are ignored");
}

/// Runs a program in the repository root with `input` on standard input: exit code, standard output and standard error.
fn run_tool(program: &str, args: &[&str], input: Option<&str>) -> Option<(i32, String, String)> {
    use std::io::Write;
    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(repo_root()).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("{program}: {e}"),
    };
    {
        let mut stdin = child.stdin.take().expect("stdin");
        if let Some(input) = input {
            let _ = stdin.write_all(input.as_bytes());
        }
    }
    let out = child.wait_with_output().expect("the tool ends");
    Some((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Whether a tool is there. A missing tool prints `NOT RUN: <tool>` and fails the test unless the sign-off run allows a run
/// without tools (the variable [`TOOLFREE`]).
fn tool_there(tool: &str, probe: &[&str]) -> bool {
    let present = run_tool(tool, probe, None).is_some_and(|(code, _, _)| code == 0);
    if !present {
        println!("NOT RUN: {tool}");
        assert!(std::env::var(TOOLFREE).as_deref() == Ok("1"), "NOT RUN: {tool} is missing and {TOOLFREE}=1 is not set (only the sign-off run may allow it)");
    }
    present
}

#[test]
fn deploy_the_checker_proves_itself_and_passes_compose_and_the_three_charts() {
    if !tool_there("node", &["--version"]) {
        return;
    }
    let (code, out, err) = run_tool("node", &["scripts/web-deploy-check.mjs", "self-test"], None).expect("node");
    assert_eq!(code, 0, "self-test: {out} {err}");
    let ok_lines = out.lines().filter(|l| l.starts_with("ok:")).count();
    println!("Deploy (self-test): exit 0, {ok_lines} ok lines, last line {:?}", out.lines().last().unwrap_or_default());

    // Compose.
    if tool_there("docker", &["compose", "version"]) {
        let (code, json, err) = run_tool("docker", &["compose", "--profile", "setup", "-f", "packaging/compose/docker-compose.yml", "config", "--format", "json"], None).expect("docker");
        assert_eq!(code, 0, "compose config: {err}");
        let (code, out, err) = run_tool("node", &["scripts/web-deploy-check.mjs", "compose"], Some(&json)).expect("node");
        assert_eq!((code, out.trim()), (0, ""), "the shipped Compose file passes the checker: {out} {err}");
        println!("Deploy (compose): the shipped file passes, exit 0, no violation");
        // Negative controls: three unsafe copies.
        let value: Value = serde_json::from_str(&json).expect("compose JSON");
        let copies: [(&str, Box<dyn Fn(&mut Value)>, &str); 3] = [
            ("privileged", Box::new(|v| v["services"]["holzwave"]["privileged"] = json!(true)), "violation: privileged"),
            ("an extra capability", Box::new(|v| v["services"]["holzwave"]["cap_add"].as_array_mut().expect("cap_add").push(json!("SYS_ADMIN"))), "violation: cap-add"),
            (
                "the container socket mounted",
                Box::new(|v| {
                    v["services"]["holzwave"]["volumes"]
                        .as_array_mut()
                        .expect("volumes")
                        .push(json!({"type": "bind", "source": "/var/run/docker.sock", "target": "/var/run/docker.sock"}))
                }),
                "violation: socket-mount",
            ),
        ];
        for (name, mutate, expected) in copies {
            let mut copy = value.clone();
            mutate(&mut copy);
            let (code, out, err) = run_tool("node", &["scripts/web-deploy-check.mjs", "compose"], Some(&copy.to_string())).expect("node");
            assert_eq!(code, 1, "{name}: refused with exit 1, got {code}: {out} {err}");
            assert!(out.contains(expected), "{name}: the checker names the rule {expected:?}: {out}");
            println!("Deploy (negative control): compose with {name}: exit 1, {:?}", out.trim());
        }
    }

    // The three chart renders.
    if tool_there("helm", &["version"]) && tool_there("kubectl", &["version", "--client"]) {
        for values in ["files", "acme", "proxy"] {
            let file = format!("packaging/helm/holzwave/ci/{values}-values.yaml");
            let (code, out, err) = run_tool("node", &["scripts/web-deploy-check.mjs", "chart-values", &file], None).expect("node");
            assert_eq!((code, out.trim()), (0, ""), "the chart rendered with {file} passes the checker: {out} {err}");
            println!("Deploy (chart): {file}: exit 0, no violation");
        }
    }
}

// =============================================================================================================================
// The spawn guard: no worker for a forbidden user id
// =============================================================================================================================

#[test]
fn spawn_guard_a_forbidden_user_id_never_reaches_the_helper_and_a_normal_one_does() {
    on_big_stack(async {
        use holzwave_server::clock::ManualClock;
        use holzwave_server::supervisor::{AttachError, HelperClient, HelperLauncher, HelperTimeouts, Supervisor, SupervisorSettings};
        use sup_env::{FakeHelper, START, account};

        // The supervisor over the helper launcher and the fake helper, with the range of the configuration.
        let (fake, end) = FakeHelper::start();
        let timeouts = HelperTimeouts { request: Duration::from_millis(400), start: Duration::from_millis(400) };
        let client = HelperClient::from_socket(end, timeouts).expect("the client").with_uid_range(20000, 29999);
        let launcher = std::sync::Arc::new(HelperLauncher::new(client));
        let clock = std::sync::Arc::new(ManualClock::new(START));
        let supervisor = Supervisor::new(launcher, SupervisorSettings::default(), clock);

        let mut refusals = Vec::new();
        for (id, uid) in [(7i64, 0u32), (8, 19999), (9, 30000)] {
            let result = supervisor.attach(account(id, uid)).await;
            match result {
                Err(AttachError::Launch(sentence)) => {
                    assert!(!sentence.is_empty() && !sentence.contains('\n'), "uid {uid}: a sentence: {sentence:?}");
                    refusals.push(format!("uid {uid}: {sentence}"));
                }
                other => panic!("uid {uid}: expected a refusal, got {:?}", other.map(|_| "attached")),
            }
        }
        assert_eq!(fake.requests(), vec![], "no request of any kind reached the helper for 0, 19999 or 30000");
        // The positive control: a normal account's request reaches the helper with the uid it owns, and the helper's request has
        // no path: the home is the helper's own business (`<home base>/<uid>`).
        let attached = supervisor.attach(account(10, 20008)).await;
        assert!(attached.is_ok(), "a normal account attaches");
        assert_eq!(fake.spawns(), vec![(20008, holzwave_spawn_helper::protocol::Mode::Serve)], "the uid the account owns, nothing else");
        println!("Spawn guard (supervisor over the helper launcher and a recording fake helper, range 20000 to 29999): {refusals:?}; recording after the three refusals: empty; control: spawn of uid 20008 only");

        // The accounts: every user id the service gave a person is inside the range, no two share one, and the workers the
        // server asked for ran as the uids the accounts own.
        let rig = Rig::start().await;
        let mut people = vec![rig.admin.clone(), rig.bob.clone()];
        for _ in 0..3 {
            let (user, _) = rig.member().await;
            people.push(user);
        }
        let uids: BTreeSet<u32> = people.iter().map(|p| p.key.uid).collect();
        assert_eq!(uids.len(), people.len(), "no two people share a user id");
        assert!(uids.iter().all(|u| edge::UID_RANGE.contains(u)), "{uids:?} are inside the range");
        let requested: BTreeSet<u32> = rig.env.ws.launcher.requests().into_iter().map(|(uid, _)| uid).collect();
        assert!(requested.is_subset(&uids), "the launcher was asked only for uids that accounts own: {requested:?} against {uids:?}");
        let homes = std::fs::read_dir(rig.env.ws.launcher.home(*uids.iter().next().expect("a uid")).parent().expect("homes")).map(|d| d.flatten().count()).unwrap_or(0);
        println!("Spawn guard (accounts): {} people with the uids {uids:?} inside the range, no duplicates; the launcher was asked for {requested:?}, a subset; {homes} homes under the homes directory", people.len());
    });
}

#[test]
fn negative_control_deploy_a_missing_tool_is_a_visible_failure_unless_the_sign_off_run_allows_it() {
    let missing = "holzwave-no-such-deployment-tool";
    if std::env::var(TOOLFREE).as_deref() == Ok("1") {
        assert!(!tool_there(missing, &["--version"]), "a missing tool is reported as absent in the tool-free run");
    } else {
        let outcome = std::panic::catch_unwind(|| tool_there(missing, &["--version"]));
        let message = outcome.expect_err("a missing tool fails by default").downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("NOT RUN: holzwave-no-such-deployment-tool"), "the failure names the tool: {message}");
    }
}

// =============================================================================================================================
// Source guards: what the text of the crates must not contain
// =============================================================================================================================

/// Words in the name of a field or a variant that mark a value as a secret.
const SECRET_WORDS: [&str; 9] = ["password", "passphrase", "secret", "token", "recovery", "key", "prf", "cookie", "verifier"];

/// Types that derive `Debug` and have a member with a secret word in its name, with the reason that is true of each. A type is
/// named as `path-suffix::Type`. An entry that no longer matches anything fails the guard, so the list cannot go stale.
const DEBUG_ALLOWED: &[(&str, &str)] = &[
    ("host/src/ai_host.rs::RemovalView", "`key_deleted` is a boolean that says a stored assistant key was removed; the view holds no key material"),
    ("host/src/state.rs::SettingsView", "`keybindings` are the keyboard shortcuts of the person (chords and action names), not a cryptographic key"),
];

/// A type that derives `Debug` and the members whose names hold a secret word.
#[derive(Debug, PartialEq, Eq)]
struct DebugFinding {
    type_name: String,
    members: Vec<String>,
}

/// The identifiers at member positions of a type body: `name:` in a struct or in a struct-like variant, and the names of the
/// variants of an enum.
fn member_names(body: &str, is_enum: bool) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    let bytes = body.as_bytes();
    let mut at = 0;
    let mut word_start: Option<usize> = None;
    while at <= bytes.len() {
        let byte = bytes.get(at).copied().unwrap_or(b' ');
        let is_word = byte.is_ascii_alphanumeric() || byte == b'_';
        if is_word && word_start.is_none() {
            word_start = Some(at);
        }
        if !is_word {
            if let Some(start) = word_start.take() {
                let word = &body[start..at];
                let next = body[at..].trim_start();
                let capitalised = word.chars().next().is_some_and(char::is_uppercase);
                if next.starts_with(':') && !next.starts_with("::") && !capitalised {
                    names.push(word.to_owned());
                } else if is_enum && depth == 1 && capitalised && body[..start].trim_end().ends_with(['{', ',']) {
                    names.push(word.to_owned());
                }
            }
            match byte {
                b'{' | b'(' | b'[' | b'<' => depth += 1,
                b'}' | b')' | b']' | b'>' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        at += 1;
    }
    names
}

/// The types of a source text that derive `Debug` and have a member named after a secret.
fn debug_findings(source: &str) -> Vec<DebugFinding> {
    let mut found = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if !(line.starts_with("#[derive(") && line.contains("Debug")) {
            index += 1;
            continue;
        }
        // The type declaration follows, after more attributes and documentation.
        let mut at = index + 1;
        while at < lines.len() {
            let next = lines[at].trim();
            if next.starts_with("#[") || next.starts_with("///") || next.is_empty() {
                at += 1;
            } else {
                break;
            }
        }
        let Some(declaration) = lines.get(at).map(|l| l.trim()) else { break };
        let after = declaration.trim_start_matches("pub(crate) ").trim_start_matches("pub ");
        let (is_enum, rest) = match (after.strip_prefix("struct "), after.strip_prefix("enum ")) {
            (Some(rest), _) => (false, rest),
            (_, Some(rest)) => (true, rest),
            _ => {
                index = at + 1;
                continue;
            }
        };
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        // The body: from the first `{` or `(` of the declaration to its match.
        let text = lines[at..].join("\n");
        let open = text.find(['{', '(', ';']);
        let body = match open {
            Some(start) if text.as_bytes()[start] != b';' => {
                let (opener, closer) = if text.as_bytes()[start] == b'{' { (b'{', b'}') } else { (b'(', b')') };
                let mut depth = 0usize;
                let mut end = text.len();
                for (offset, byte) in text.bytes().enumerate().skip(start) {
                    if byte == opener {
                        depth += 1;
                    } else if byte == closer {
                        depth -= 1;
                        if depth == 0 {
                            end = offset;
                            break;
                        }
                    }
                }
                text[start..=end.min(text.len() - 1)].to_owned()
            }
            _ => String::new(),
        };
        let members: Vec<String> = member_names(&body, is_enum)
            .into_iter()
            .filter(|m| SECRET_WORDS.iter().any(|w| m.to_ascii_lowercase().contains(w)))
            .collect();
        if !members.is_empty() {
            found.push(DebugFinding { type_name: name, members });
        }
        index = at + 1;
    }
    found
}

fn sources_below(dir: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs") {
                let shown = path.strip_prefix(repo_root()).unwrap_or(&path).display().to_string();
                files.push((shown, read(&path)));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn negative_control_source_guards_derived_debug_with_a_secret_member_is_flagged_and_a_manual_impl_is_not() {
    let derived = "#[derive(Debug, Clone)]\npub struct Login {\n    pub name: String,\n    pub password: String,\n}\n";
    assert_eq!(debug_findings(derived), [DebugFinding { type_name: "Login".to_owned(), members: vec!["password".to_owned()] }]);
    let manual = "pub struct Login {\n    pub name: String,\n    pub password: String,\n}\nimpl std::fmt::Debug for Login {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        f.write_str(\"Login(..)\")\n    }\n}\n";
    assert!(debug_findings(manual).is_empty(), "a manual implementation that prints a marker is not flagged");
    let variant = "#[derive(Debug)]\nenum Held {\n    Totp([u8; 20]),\n    RecoveryCode(String),\n    Passkey { prf_salt: Vec<u8> },\n}\n";
    let found = debug_findings(variant);
    assert_eq!(found.len(), 1);
    assert!(found[0].members.contains(&"RecoveryCode".to_owned()) && found[0].members.contains(&"prf_salt".to_owned()), "{found:?}");
    let clean = "#[derive(Debug)]\nstruct Point {\n    x: i32,\n    y: i32,\n}\n";
    assert!(debug_findings(clean).is_empty());
    let nested = "#[derive(Debug)]\nstruct Wrapper {\n    inner: Option<Box<[u8]>>,\n    cookie_jar: Vec<String>,\n}\n";
    assert_eq!(debug_findings(nested)[0].members, ["cookie_jar"]);
}

#[test]
fn source_guards_no_type_that_derives_debug_in_host_or_the_spawn_helper_has_a_member_named_after_a_secret() {
    let mut findings = Vec::new();
    let mut used = BTreeSet::new();
    let mut scanned = 0;
    for crate_dir in ["host/src", "spawn-helper/src"] {
        for (file, text) in sources_below(&repo_root().join(crate_dir)) {
            scanned += 1;
            for finding in debug_findings(&text) {
                let id = format!("{file}::{}", finding.type_name);
                if DEBUG_ALLOWED.iter().any(|(allowed, _)| *allowed == id) {
                    used.insert(id);
                } else {
                    findings.push(format!("{id}: {:?}", finding.members));
                }
            }
        }
    }
    let stale: Vec<&str> = DEBUG_ALLOWED.iter().map(|(id, _)| *id).filter(|id| !used.contains(*id)).collect();
    println!("Source guards (Debug): {scanned} files in host/src and spawn-helper/src; {} allowed types: {:?}; findings: {findings:?}", DEBUG_ALLOWED.len(), DEBUG_ALLOWED);
    assert!(findings.is_empty(), "a derived Debug on a type with a secret member:\n{}", findings.join("\n"));
    assert!(stale.is_empty(), "allow-list entries that match nothing: {stale:?}");
}

/// Files of `server/src` that may print: the command line (its sentences and its three one-line answers).
/// An entry says why.
const PRINT_ALLOWED: &[(&str, &str)] = &[
    ("server/src/cli.rs", "the command line: one sentence on standard error for a refusal, and the answers of keygen, check-config and healthcheck"),
];

/// The lines of a source text that print through a macro that goes to standard output or standard error. Comments are not code.
fn print_lines(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let code = line.trim_start();
            !code.starts_with("//") && ["println!", "eprintln!", "dbg!", "print!", "eprint!"].iter().any(|m| code.contains(m))
        })
        .map(|(n, _)| n + 1)
        .collect()
}

#[test]
fn negative_control_source_guards_a_print_macro_in_code_is_flagged_and_one_in_a_comment_is_not() {
    assert_eq!(print_lines("fn a() {\n    println!(\"x\");\n}\n"), [2]);
    assert_eq!(print_lines("fn a() {\n    dbg!(1);\n    eprintln!(\"x\");\n}\n"), [2, 3]);
    assert!(print_lines("// println!(\"x\")\n/// eprintln!(\"y\")\nfn a() {}\n").is_empty());
}

#[test]
fn source_guards_the_server_prints_only_where_the_command_line_does() {
    let mut findings = Vec::new();
    let mut used = BTreeSet::new();
    let mut scanned = 0;
    for (file, text) in sources_below(&server_root().join("src")) {
        scanned += 1;
        let lines = print_lines(&text);
        if lines.is_empty() {
            continue;
        }
        if PRINT_ALLOWED.iter().any(|(allowed, _)| *allowed == file) {
            used.insert(file);
        } else {
            findings.push(format!("{file}: lines {lines:?}"));
        }
    }
    assert!(findings.is_empty(), "a print macro outside the command line:\n{}", findings.join("\n"));
    let stale: Vec<&str> = PRINT_ALLOWED.iter().map(|(f, _)| *f).filter(|f| !used.contains(*f)).collect();
    assert!(stale.is_empty(), "allow-list entries that print nothing (a file that no longer prints must leave the list): {stale:?}");
    println!("Source guards (print): {scanned} files in server/src; allowed: {PRINT_ALLOWED:?}");
}

/// The files that must not know where a person's home is: the HTTP layer, the sign-in code, the accounts service, the audit log
/// and the store. The home of a person is the helper's and the supervisor's business.
fn home_unaware_files() -> Vec<(String, String)> {
    let root = server_root().join("src");
    let mut files = Vec::new();
    for part in ["http", "auth"] {
        files.extend(sources_below(&root.join(part)));
    }
    for single in ["accounts.rs", "audit.rs", "store.rs"] {
        let path = root.join(single);
        if path.exists() {
            files.push((format!("server/src/{single}"), read(&path)));
        } else if path.with_extension("").is_dir() {
            files.extend(sources_below(&path.with_extension("")));
        }
    }
    files
}

/// The lines that name the function that makes a home path, the directory that holds the homes, or a home directory by path.
fn home_path_lines(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let code = line.trim_start();
            !code.starts_with("//") && (code.contains("home_path") || code.contains("\"homes\"") || code.contains("/homes/") || code.contains("home_base"))
        })
        .map(|(n, _)| n + 1)
        .collect()
}

#[test]
fn negative_control_source_guards_a_home_path_in_the_http_layer_is_flagged() {
    assert_eq!(home_path_lines("let h = home_path(base, uid);\n"), [1]);
    assert_eq!(home_path_lines("let h = data.join(\"homes\");\n"), [1]);
    assert!(home_path_lines("// the homes directory is the helper's\nlet x = 1;\n").is_empty());
}

#[test]
fn source_guards_the_http_sign_in_accounts_audit_and_store_code_never_names_a_home() {
    let files = home_unaware_files();
    assert!(files.len() > 10, "the guard reads the files: {}", files.len());
    let findings: Vec<String> = files.iter().filter_map(|(file, text)| {
        let lines = home_path_lines(text);
        (!lines.is_empty()).then(|| format!("{file}: lines {lines:?}"))
    }).collect();
    assert!(findings.is_empty(), "a home path in a layer that must not know it:\n{}", findings.join("\n"));
    println!("Source guards (home path): {} files of server/src/http, server/src/auth, accounts, audit and store name no home directory", files.len());
}

/// What is wrong with the way `run` and `run_overrides` build the overrides of the running server, and with the declaration of
/// the fields of `Overrides`: `run` takes the overrides from `run_overrides()`, which is `Overrides::default()` and nothing
/// else, no field is set in either, and no `cfg(` attribute stands within three lines of a field of the struct.
fn overrides_problems(source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let body_of = |name: &str| -> Option<String> {
        let start = source.find(&format!("fn {name}("))?;
        let open = start + source[start..].find('{')?;
        let mut depth = 0usize;
        for (offset, byte) in source.bytes().enumerate().skip(open) {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(source[open + 1..offset].to_owned());
                    }
                }
                _ => {}
            }
        }
        None
    };
    let fields = ["listen", "token_writer", "grace", "clock", "oidc_client"];
    for name in ["run", "run_overrides"] {
        match body_of(name) {
            None => problems.push(format!("the function {name} is missing")),
            Some(body) => {
                for field in fields {
                    if body.contains(&format!("{field}:")) || body.contains(&format!(".{field} =")) || body.contains(&format!(".{field}=")) {
                        problems.push(format!("{name} sets the override {field}"));
                    }
                }
                if body.contains("cfg!(") || body.contains("#[cfg") {
                    problems.push(format!("{name} selects something by cfg"));
                }
            }
        }
    }
    if let Some(body) = body_of("run") {
        if !body.contains("run_overrides()") {
            problems.push("run does not take its overrides from run_overrides()".to_owned());
        }
    }
    if let Some(body) = body_of("run_overrides") {
        if body.trim() != "Overrides::default()" {
            problems.push("run_overrides is not Overrides::default()".to_owned());
        }
    }
    // No attribute that selects by configuration stands near a field of the struct.
    let lines: Vec<&str> = source.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let code = line.trim_start();
        if fields.iter().any(|f| code.starts_with(&format!("pub {f}:"))) {
            let from = index.saturating_sub(3);
            let to = (index + 3).min(lines.len() - 1);
            if lines[from..=to].iter().any(|l| l.contains("cfg(") && !l.trim_start().starts_with("//")) {
                problems.push(format!("a cfg attribute stands within three lines of the field on line {}", index + 1));
            }
        }
    }
    problems
}

#[test]
fn negative_control_source_guards_run_that_sets_a_field_or_a_cfg_beside_a_field_is_flagged() {
    let good = "pub struct Overrides {\n    pub listen: Option<u16>,\n    pub oidc_client: Option<u8>,\n}\nfn run_overrides() -> Overrides {\n    Overrides::default()\n}\npub async fn run(p: u8) {\n    let o = run_overrides();\n}\n";
    assert!(overrides_problems(good).is_empty(), "{:?}", overrides_problems(good));
    let sets = good.replace("let o = run_overrides();", "let o = run_overrides();\n    let x = Overrides { oidc_client: Some(1), ..o };");
    assert!(overrides_problems(&sets).iter().any(|p| p.contains("sets the override oidc_client")), "{:?}", overrides_problems(&sets));
    let cfg = good.replace("    pub listen", "    #[cfg(test)]\n    pub listen");
    assert!(overrides_problems(&cfg).iter().any(|p| p.contains("cfg attribute")), "{:?}", overrides_problems(&cfg));
    let other = good.replace("Overrides::default()", "Overrides { listen: None }");
    assert!(!overrides_problems(&other).is_empty());
    let skipped = good.replace("let o = run_overrides();", "let o = Overrides::default();");
    assert!(overrides_problems(&skipped).iter().any(|p| p.contains("does not take")), "{:?}", overrides_problems(&skipped));
}

#[test]
fn source_guards_the_running_server_is_built_from_the_default_overrides_and_no_attribute_selects_one() {
    let source = read(&server_root().join("src/supervisor/serve.rs"));
    let problems = overrides_problems(&source);
    assert!(problems.is_empty(), "{problems:?}");
    println!("Source guards (overrides): run takes run_overrides(), which is Overrides::default(); no field is set and no cfg stands near a field");
}
