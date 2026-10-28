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
