//! The HTTP application layer: who the caller is, whether the request may change anything, whether it came to the right name,
//! and what every response says about itself.

#[path = "support/edge.rs"]
mod edge;

use axum::http::{HeaderMap, HeaderValue, Method};
use edge::{ABSOLUTE, HOST, IDLE, NOW, ORIGIN, Options, Req, STEP_UP, Signed, TestApp, probe_parts};
use holzwave_server::http::cookies;
use holzwave_server::store::Role;
use serde_json::{Value, json};

/// The header names of every answer and their exact values (the content security policy is checked on its own).
const FIXED: [(&str, &str); 7] = [
    ("strict-transport-security", "max-age=31536000"),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    ("cache-control", "no-store"),
    ("content-type", "application/json"),
];

fn shown(value: &Value, csrf_len: bool) -> String {
    let mut value = value.clone();
    if csrf_len && let Some(csrf) = value.get("csrf").and_then(Value::as_str).map(str::len) {
        value["csrf"] = json!(format!("<{csrf} chars>"));
    }
    value.to_string()
}

#[tokio::test]
async fn tracer_anonymous() {
    let app = TestApp::new();
    app.create_account("alice", Role::Member);
    let first = app.send(app.req(Method::GET, "/api/session")).await;
    assert_eq!(first.status, 200);
    println!("Tracer: anonymous with no administrator = {}", shown(&first.json(), true));
    assert_eq!(
        first.json(),
        json!({
            "state": "anonymous",
            "host": HOST,
            "bootstrap": true,
            "methods": { "passkey": true, "password": true, "oidc": [{ "id": "corp", "name": "Company" }] }
        })
    );
    assert_eq!(first.header("cache-control").as_deref(), Some("no-store"));

    app.create_account("root", Role::Admin);
    let second = app.send(app.req(Method::GET, "/api/session")).await;
    println!("Tracer: anonymous with an administrator = {}", shown(&second.json(), true));
    assert_eq!(second.json()["bootstrap"], json!(false));
    assert_eq!(second.json().as_object().expect("an object").len(), 4);
}

#[tokio::test]
async fn tracer_signed_in() {
    let app = TestApp::new();
    app.create_account("alice", Role::Member);
    let alice = app.sign_in("alice");
    let first = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await;
    assert_eq!(first.status, 200);
    let body = first.json();
    println!("Tracer: signed in at the start = {}", shown(&body, true));
    assert_eq!(body["state"], json!("signed_in"));
    assert_eq!(body["host"], json!(HOST));
    assert_eq!(body["csrf"].as_str().expect("a token").len(), 43);
    assert_eq!(body["csrf"].as_str(), Some(alice.csrf.as_str()));
    let account = &body["account"];
    assert_eq!(account["username"], json!("alice"));
    assert_eq!(account["display_name"], json!("alice"));
    assert_eq!(account["role"], json!("member"));
    assert_eq!(account["id"], json!(alice.account.get()));
    assert_eq!(account.as_object().expect("an object").len(), 4);
    assert_eq!(body["seconds"], json!(IDLE));
    assert_eq!(body["step_up"], json!(STEP_UP));
    assert_eq!(body.as_object().expect("an object").len(), 6, "{body}");
    assert_eq!(first.header("cache-control").as_deref(), Some("no-store"));

    app.clock.advance(300);
    let later = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await.json();
    println!("Tracer: signed in 300 s later = {}", shown(&later, true));
    assert_eq!(later["seconds"], json!(1_500));
    assert_eq!(later["step_up"], json!(0));
    assert_eq!(later["state"], json!("signed_in"));

    app.view.set_locked(alice.account, true);
    let locked = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await.json();
    assert_eq!(locked["state"], json!("locked"));
    assert_eq!(locked["csrf"], json!(alice.csrf));
}

#[tokio::test]
async fn tracer_seconds_is_the_nearer_end() {
    let app = TestApp::new();
    app.create_account("alice", Role::Member);
    let alice = app.sign_in("alice");
    // Keep the session busy until the absolute end is nearer than the idle end.
    let mut elapsed = 0;
    while elapsed + 1_000 < ABSOLUTE - 600 {
        app.clock.advance(1_000);
        elapsed += 1_000;
        let answer = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await;
        assert_eq!(answer.json()["state"], json!("signed_in"));
    }
    let left = ABSOLUTE - elapsed;
    let body = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await.json();
    println!("Tracer: absolute end nearer = {} (the absolute end is {left} s away)", body["seconds"]);
    assert_eq!(body["seconds"], json!(left));
    assert!(left < IDLE);
    assert_eq!(NOW + elapsed + left, NOW + ABSOLUTE);
}

#[tokio::test]
async fn tracer_headers_are_on_every_answer() {
    let app = TestApp::new();
    app.create_account("alice", Role::Member);
    let alice = app.sign_in("alice");
    let answers = [
        app.send(app.req(Method::GET, "/api/session")).await,
        app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await,
        app.send(app.req(Method::GET, "/nowhere")).await,
        app.send(app.req(Method::POST, "/api/session")).await,
    ];
    println!("Headers: {}", answers[0].header_names().join(", "));
    for answer in &answers {
        for (name, value) in FIXED {
            assert_eq!(answer.header(name).as_deref(), Some(value), "{name} on {}", answer.status);
        }
        let policy = answer.header("permissions-policy").expect("a permissions policy");
        for denied in ["camera=()", "microphone=()", "geolocation=()", "payment=()", "usb=()"] {
            assert!(policy.contains(denied), "{policy}");
        }
        assert!(policy.contains("clipboard-read=(self)") && policy.contains("clipboard-write=(self)"));
        assert!(policy.contains("publickey-credentials-get=(self)"));
        assert!(answer.headers.keys().all(|n| !n.as_str().starts_with("access-control-")));

        let csp = answer.header("content-security-policy").expect("a policy");
        assert!(csp.contains(&format!("connect-src 'self' wss://{HOST};")), "{csp}");
        assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'") && csp.contains("object-src 'none'"));
        assert!(csp.contains("script-src 'self';") && csp.contains("base-uri 'none'") && csp.contains("form-action 'self'"));
        assert!(!csp.contains("unsafe") && !csp.contains('*') && !csp.contains("http:"));
        // Every host the policy names is the public one.
        for word in csp.split([' ', ';']) {
            if word.contains("://") || word.contains('.') {
                assert!(word == format!("wss://{HOST}"), "a foreign source in the policy: {word}");
            }
        }
    }
    let _ = ORIGIN;
}

// ---------------------------------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------------------------------

/// An application with the probe routes and a member called alice who is signed in.
fn probed() -> (TestApp, Signed) {
    let app = TestApp::with_parts(probe_parts());
    app.create_account("alice", Role::Member);
    let alice = app.sign_in("alice");
    (app, alice)
}

async fn code(app: &TestApp, req: Req) -> u16 {
    app.send(req).await.status.as_u16()
}

/// Sends every row and returns `status` per row in a compact form; panics at the first row that differs from `want`.
async fn table(app: &TestApp, label: &str, rows: Vec<(String, Req, u16)>) -> String {
    let mut seen = Vec::new();
    for (name, req, want) in rows {
        let got = app.send(req).await;
        assert_eq!(got.status.as_u16(), want, "{label}: {name}: {}", got.text());
        seen.push(format!("{name}={want}"));
    }
    println!("{label}: {}", seen.join(" | "));
    seen.join(" | ")
}

fn public_get(app: &TestApp, host: Option<&str>) -> Req {
    let req = Req::new(Method::GET, "/t/public");
    match host {
        Some(host) => req.header("host", host),
        None => req,
    }
    .without("x-none")
    .header("x-probe", &app.app.origin.text().len().to_string())
}

// ---------------------------------------------------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn host_the_public_name_passes_in_any_case() {
    let (app, _) = probed();
    let rows = vec![
        ("hw.example.test".to_owned(), public_get(&app, Some("hw.example.test")), 200),
        ("HW.Example.TEST".to_owned(), public_get(&app, Some("HW.Example.TEST")), 200),
        ("hw.example.test:443".to_owned(), public_get(&app, Some("hw.example.test:443")), 200),
    ];
    table(&app, "Host table", rows).await;
}

#[tokio::test]
async fn host_other_names_are_misdirected() {
    let (app, _) = probed();
    let hosts = [
        "evil.example",
        "hw.example.test.evil.example",
        "evilhw.example.test",
        "user@hw.example.test",
        "hw.example.test@evil.example",
        "hw.example.test/",
        "hw.example.test/x",
        "hw.example.test?x",
        "hw.example.test#x",
        "hw.example.test\\evil",
        "hw.example.test:8443",
        "hw.example.test:",
        "hw.example.test:0443",
        "hw.example.test:443:443",
        "hw.example.test ",
        " hw.example.test",
        "",
        "hw.example.test.",
        "[::1]",
    ];
    let rows = hosts.iter().map(|h| (format!("{h:?}"), public_get(&app, Some(h)), 421)).collect();
    table(&app, "Host table (refused)", rows).await;
}

#[tokio::test]
async fn host_missing_and_repeated() {
    let (app, _) = probed();
    let rows = vec![
        ("no Host".to_owned(), public_get(&app, None), 421),
        (
            "two Host".to_owned(),
            public_get(&app, Some("hw.example.test")).header("host", "hw.example.test"),
            421,
        ),
        (
            "good then foreign".to_owned(),
            public_get(&app, Some("hw.example.test")).header("host", "evil.example"),
            421,
        ),
    ];
    table(&app, "Host table (count)", rows).await;
}

#[tokio::test]
async fn host_with_a_port_in_the_public_origin() {
    let app = TestApp::with(Options { url: "https://hw.example.test:8443".to_owned(), parts: probe_parts(), ..Options::default() });
    let rows = vec![
        ("hw.example.test:8443".to_owned(), public_get(&app, Some("hw.example.test:8443")), 200),
        ("HW.EXAMPLE.TEST:8443".to_owned(), public_get(&app, Some("HW.EXAMPLE.TEST:8443")), 200),
        ("hw.example.test".to_owned(), public_get(&app, Some("hw.example.test")), 421),
        ("hw.example.test:443".to_owned(), public_get(&app, Some("hw.example.test:443")), 421),
        ("hw.example.test:8444".to_owned(), public_get(&app, Some("hw.example.test:8444")), 421),
    ];
    table(&app, "Host table (port 8443)", rows).await;
    let policy = app.send(public_get(&app, Some("hw.example.test:8443"))).await.header("content-security-policy").expect("policy");
    assert!(policy.contains("connect-src 'self' wss://hw.example.test:8443;"), "{policy}");
}

#[tokio::test]
async fn host_http2_authority_in_the_target() {
    let (app, _) = probed();
    let h2 = |uri: &str| Req::new(Method::GET, uri);
    let rows = vec![
        ("authority matches, no Host".to_owned(), h2("https://hw.example.test/t/public"), 200),
        ("authority differs, no Host".to_owned(), h2("https://evil.example/t/public"), 421),
        ("authority with user information".to_owned(), h2("https://user@hw.example.test/t/public"), 421),
        (
            "authority matches, Host differs".to_owned(),
            h2("https://hw.example.test/t/public").header("host", "evil.example"),
            421,
        ),
        (
            "authority differs, Host matches".to_owned(),
            h2("https://evil.example/t/public").header("host", "hw.example.test"),
            421,
        ),
        (
            "authority and Host match".to_owned(),
            h2("https://hw.example.test/t/public").header("host", "hw.example.test"),
            200,
        ),
    ];
    table(&app, "Host table (HTTP/2 style)", rows).await;
}

#[tokio::test]
async fn host_is_checked_before_routing_and_on_every_kind() {
    let (app, alice) = probed();
    let wrong = |method: Method, path: &str| Req::new(method, path).header("host", "evil.example");
    let rows = vec![
        ("unknown path".to_owned(), wrong(Method::GET, "/nowhere"), 421),
        ("api/session".to_owned(), wrong(Method::GET, "/api/session"), 421),
        ("session part with a live session".to_owned(), wrong(Method::GET, "/t/session").cookie(&alice.cookie), 421),
        ("socket part".to_owned(), wrong(Method::GET, "/t/socket"), 421),
        ("upload part".to_owned(), wrong(Method::PUT, "/t/upload").signed(&alice), 421),
        ("options".to_owned(), wrong(Method::OPTIONS, "/t/public"), 421),
    ];
    table(&app, "Host table (before routing)", rows).await;
    let answer = app.send(wrong(Method::GET, "/nowhere")).await;
    assert_eq!(answer.json(), json!({ "code": "bad_request" }));
    assert!(answer.headers.get("set-cookie").is_none());
}

// ---------------------------------------------------------------------------------------------------------------------
// Origin
// ---------------------------------------------------------------------------------------------------------------------

fn session_post(app: &TestApp, who: &Signed) -> Req {
    app.change_as(Method::POST, "/t/session", who)
}

#[tokio::test]
async fn origin_the_public_origin_passes() {
    let (app, alice) = probed();
    let rows = vec![("https://hw.example.test".to_owned(), session_post(&app, &alice), 200)];
    table(&app, "Origin table (exact)", rows).await;
}

#[tokio::test]
async fn origin_every_other_spelling_is_forbidden() {
    let (app, alice) = probed();
    let origins = [
        "http://hw.example.test",
        "https://hw.example.test:444",
        "https://hw.example.test:443",
        "https://hw.example.test/",
        "https://hw.example.test/x",
        "https://hw.example.test.evil.example",
        "https://evilhw.example.test",
        "https://HW.example.test",
        "https://hw.example.test ",
        " https://hw.example.test",
        "hw.example.test",
        "null",
        "",
        "*",
        "https://evil.example",
    ];
    let rows = origins.iter().map(|o| (format!("{o:?}"), session_post(&app, &alice).set("origin", o), 403)).collect();
    table(&app, "Origin table (refused)", rows).await;
}

#[tokio::test]
async fn origin_repeated_headers_are_forbidden() {
    let (app, alice) = probed();
    let rows = vec![
        ("two right".to_owned(), session_post(&app, &alice).header("origin", ORIGIN), 403),
        ("right then foreign".to_owned(), session_post(&app, &alice).header("origin", "https://evil.example"), 403),
        (
            "foreign then right".to_owned(),
            session_post(&app, &alice).without("origin").header("origin", "https://evil.example").header("origin", ORIGIN),
            403,
        ),
    ];
    table(&app, "Origin table (repeated)", rows).await;
}

#[tokio::test]
async fn origin_absent_needs_same_origin_fetch_metadata() {
    let (app, alice) = probed();
    let bare = |site: Option<&str>| {
        let req = session_post(&app, &alice).without("origin");
        match site {
            Some(site) => req.header("sec-fetch-site", site),
            None => req,
        }
    };
    let rows = vec![
        ("same-origin".to_owned(), bare(Some("same-origin")), 200),
        ("same-site".to_owned(), bare(Some("same-site")), 403),
        ("cross-site".to_owned(), bare(Some("cross-site")), 403),
        ("none".to_owned(), bare(Some("none")), 403),
        ("no header".to_owned(), bare(None), 403),
        ("Same-Origin".to_owned(), bare(Some("Same-Origin")), 403),
        ("empty".to_owned(), bare(Some("")), 403),
        ("twice".to_owned(), bare(Some("same-origin")).header("sec-fetch-site", "same-origin"), 403),
    ];
    table(&app, "Origin table (fetch metadata)", rows).await;
}

#[tokio::test]
async fn origin_present_decides_alone() {
    let (app, alice) = probed();
    let rows = vec![
        (
            "right Origin, cross-site".to_owned(),
            session_post(&app, &alice).header("sec-fetch-site", "cross-site"),
            200,
        ),
        (
            "wrong Origin, same-origin".to_owned(),
            session_post(&app, &alice).set("origin", "https://evil.example").header("sec-fetch-site", "same-origin"),
            403,
        ),
        (
            "null Origin, same-origin".to_owned(),
            session_post(&app, &alice).set("origin", "null").header("sec-fetch-site", "same-origin"),
            403,
        ),
    ];
    table(&app, "Origin table (Origin wins)", rows).await;
}

#[tokio::test]
async fn origin_is_not_checked_for_a_read() {
    let (app, alice) = probed();
    let rows = vec![
        ("GET foreign Origin, public".to_owned(), app.req(Method::GET, "/t/public").header("origin", "https://evil.example"), 200),
        (
            "GET foreign Origin, session".to_owned(),
            app.req(Method::GET, "/t/session").header("origin", "https://evil.example").cookie(&alice.cookie),
            200,
        ),
        ("GET null Origin".to_owned(), app.req(Method::GET, "/t/public").header("origin", "null"), 200),
    ];
    table(&app, "Origin table (reads)", rows).await;
}

#[tokio::test]
async fn origin_is_checked_on_a_public_change_too() {
    let (app, _) = probed();
    let rows = vec![
        ("public POST right Origin".to_owned(), app.change(Method::POST, "/t/public"), 200),
        ("public POST foreign Origin".to_owned(), app.change(Method::POST, "/t/public").set("origin", "https://evil.example"), 403),
        ("public POST no Origin".to_owned(), app.change(Method::POST, "/t/public").without("origin"), 403),
        (
            "public POST no Origin, same-origin".to_owned(),
            app.change(Method::POST, "/t/public").without("origin").header("sec-fetch-site", "same-origin"),
            200,
        ),
    ];
    table(&app, "Origin table (public changes)", rows).await;
}

#[tokio::test]
async fn origin_is_checked_before_the_session_and_every_changing_method_is_covered() {
    let (app, alice) = probed();
    // A foreign origin without a session is 403 (the origin comes first), not 401.
    assert_eq!(code(&app, app.change(Method::POST, "/t/session").set("origin", "https://evil.example")).await, 403);
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let wrong = app.change_as(method.clone(), "/t/session", &alice).set("origin", "https://evil.example");
        assert_eq!(code(&app, wrong).await, 403, "{method}");
        let right = app.change_as(method.clone(), "/t/session", &alice);
        assert_eq!(code(&app, right).await, 200, "{method}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Content type and body
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn content_type_json_with_parameters_passes_on_every_changing_method() {
    let (app, alice) = probed();
    let mut rows = Vec::new();
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for value in ["application/json", "application/json; charset=utf-8", "Application/JSON", "application/json ;charset=utf-8"] {
            rows.push((
                format!("{method} {value}"),
                app.change_as(method.clone(), "/t/session", &alice).set("content-type", value),
                200,
            ));
        }
    }
    table(&app, "Content type (passes)", rows).await;
}

#[tokio::test]
async fn content_type_everything_else_is_a_bad_request() {
    let (app, alice) = probed();
    let values = [
        "text/plain",
        "application/x-www-form-urlencoded",
        "multipart/form-data; boundary=x",
        "application/jsonx",
        "application/json-patch+json",
        "application/octet-stream",
        "text/json",
        "application/",
        "json",
        "",
    ];
    let rows = values.iter().map(|v| (format!("{v:?}"), app.change_as(Method::POST, "/t/session", &alice).set("content-type", v), 400)).collect();
    table(&app, "Content type (refused)", rows).await;
}

#[tokio::test]
async fn content_type_missing_and_repeated() {
    let (app, alice) = probed();
    let rows = vec![
        ("none".to_owned(), app.change_as(Method::POST, "/t/session", &alice).without("content-type"), 400),
        (
            "two".to_owned(),
            app.change_as(Method::POST, "/t/session", &alice).header("content-type", "application/json"),
            400,
        ),
        (
            "json then text".to_owned(),
            app.change_as(Method::POST, "/t/session", &alice).header("content-type", "text/plain"),
            400,
        ),
        ("a delete without a body still needs it".to_owned(), app.change_as(Method::DELETE, "/t/session", &alice).without("content-type"), 400),
    ];
    table(&app, "Content type (count)", rows).await;
    let answer = app.send(app.change_as(Method::POST, "/t/session", &alice).set("content-type", "text/plain")).await;
    assert_eq!(answer.json(), json!({ "code": "bad_request" }));
}

#[tokio::test]
async fn content_type_a_reading_needs_none() {
    let (app, alice) = probed();
    let rows = vec![
        ("GET no content type".to_owned(), app.req(Method::GET, "/t/session").cookie(&alice.cookie), 200),
        ("GET text/plain".to_owned(), app.req(Method::GET, "/t/session").cookie(&alice.cookie).header("content-type", "text/plain"), 200),
    ];
    table(&app, "Content type (reads)", rows).await;
}

#[tokio::test]
async fn content_type_the_upload_part_takes_octet_stream_and_refuses_json() {
    let (app, alice) = probed();
    let upload = |content_type: &str| {
        app.change_as(Method::PUT, "/t/upload", &alice).set("content-type", content_type).bytes(vec![7u8; 100])
    };
    let rows = vec![
        ("octet-stream".to_owned(), upload("application/octet-stream"), 200),
        ("octet-stream with a parameter".to_owned(), upload("application/octet-stream; x=y"), 200),
        ("json".to_owned(), upload("application/json"), 400),
        ("text/plain".to_owned(), upload("text/plain"), 400),
        (
            "no content type".to_owned(),
            app.change_as(Method::PUT, "/t/upload", &alice).without("content-type").bytes(vec![7u8; 100]),
            400,
        ),
    ];
    table(&app, "Content type (upload)", rows).await;
    // The upload part keeps the Origin and the token checks.
    let octet = |req: Req| req.set("content-type", "application/octet-stream").bytes(vec![1; 10]);
    assert_eq!(code(&app, octet(app.change_as(Method::PUT, "/t/upload", &alice).set("origin", "https://evil.example"))).await, 403);
    assert_eq!(code(&app, octet(app.change_as(Method::PUT, "/t/upload", &alice).without("x-csrf-token"))).await, 403);
    assert_eq!(code(&app, octet(app.change(Method::PUT, "/t/upload"))).await, 401);
}

const CAP: usize = 64 * 1024;

#[tokio::test]
async fn body_at_the_cap_passes_and_one_byte_more_does_not() {
    let (app, alice) = probed();
    let rows = vec![
        ("65536 bytes".to_owned(), app.change_as(Method::POST, "/t/session", &alice).bytes(vec![b' '; CAP]), 200),
        ("65537 bytes".to_owned(), app.change_as(Method::POST, "/t/session", &alice).bytes(vec![b' '; CAP + 1]), 400),
        ("65536 declared".to_owned(), app.change_as(Method::POST, "/t/session", &alice).bytes(vec![b' '; CAP]).header("content-length", "65536"), 200),
        ("65537 declared".to_owned(), app.change_as(Method::POST, "/t/session", &alice).bytes(vec![b' '; CAP + 1]).header("content-length", "65537"), 400),
        ("public 65537".to_owned(), app.change(Method::POST, "/t/public").bytes(vec![b' '; CAP + 1]), 400),
    ];
    table(&app, "Body", rows).await;
}

#[tokio::test]
async fn body_a_streamed_body_over_the_cap_is_refused_without_a_declared_length() {
    let (app, alice) = probed();
    let frames = |sizes: &[usize]| sizes.iter().map(|n| vec![b' '; *n]).collect::<Vec<_>>();
    let rows = vec![
        ("3 frames, 65536 in all".to_owned(), app.change_as(Method::POST, "/t/session", &alice).chunks(frames(&[30_000, 30_000, 5_536])), 200),
        ("3 frames, 65537 in all".to_owned(), app.change_as(Method::POST, "/t/session", &alice).chunks(frames(&[30_000, 30_000, 5_537])), 400),
        ("many small frames over".to_owned(), app.change_as(Method::POST, "/t/session", &alice).chunks(frames(&[1_000; 70])), 400),
        (
            "chunked header, one big frame".to_owned(),
            app.change_as(Method::POST, "/t/session", &alice).header("transfer-encoding", "chunked").bytes(vec![b' '; 5 * CAP]),
            400,
        ),
    ];
    table(&app, "Body (streamed)", rows).await;
}

#[tokio::test]
async fn body_the_upload_part_has_no_cap_here() {
    let (app, alice) = probed();
    let big = app
        .change_as(Method::PUT, "/t/upload", &alice)
        .set("content-type", "application/octet-stream")
        .bytes(vec![0u8; 3 * CAP]);
    let answer = app.send(big).await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["bytes"], json!(3 * CAP));
}

#[tokio::test]
async fn body_a_declared_length_that_lies_or_repeats_is_refused() {
    let (app, alice) = probed();
    let rows = vec![
        ("declares more than it sends".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "50"), 400),
        ("declares less than it sends".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "1"), 400),
        ("declares exactly".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "2"), 200),
        ("declares twice".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "2").header("content-length", "2"), 400),
        ("declares a sign".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "+2"), 400),
        ("declares a number too big".to_owned(), app.change_as(Method::POST, "/t/session", &alice).header("content-length", "99999999999999999999999"), 400),
    ];
    table(&app, "Body (declared length)", rows).await;
}

// ---------------------------------------------------------------------------------------------------------------------
// The anti-forgery token
// ---------------------------------------------------------------------------------------------------------------------

fn swap_case(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }).collect()
}

fn one_off(text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    chars[10] = if chars[10] == 'A' { 'B' } else { 'A' };
    chars.into_iter().collect()
}

#[tokio::test]
async fn csrf_a_change_without_the_right_token_is_forbidden() {
    let (app, alice) = probed();
    let other = app.sign_in("alice");
    let post = || app.change(Method::POST, "/t/session").cookie(&alice.cookie);
    let rows = vec![
        ("no token".to_owned(), post(), 403),
        ("empty token".to_owned(), post().csrf(""), 403),
        ("one character off".to_owned(), post().csrf(&one_off(&alice.csrf)), 403),
        ("token of another session of the same account".to_owned(), post().csrf(&other.csrf), 403),
        ("the right token in the wrong case".to_owned(), post().csrf(&swap_case(&alice.csrf)), 403),
        ("the right token with a space".to_owned(), post().csrf(&format!("{} ", alice.csrf)), 403),
        ("a shorter prefix".to_owned(), post().csrf(&alice.csrf[..42]), 403),
        ("the cookie value as the token".to_owned(), post().csrf(&alice.cookie), 403),
        ("the right token".to_owned(), post().csrf(&alice.csrf), 200),
        ("the other session with its own token".to_owned(), app.change(Method::POST, "/t/session").cookie(&other.cookie).csrf(&other.csrf), 200),
    ];
    table(&app, "CSRF", rows).await;
    assert_ne!(alice.csrf, other.csrf);
}

#[tokio::test]
async fn csrf_repeated_token_headers_are_forbidden() {
    let (app, alice) = probed();
    let rows = vec![
        ("right twice".to_owned(), session_post(&app, &alice).csrf(&alice.csrf), 403),
        ("right then wrong".to_owned(), session_post(&app, &alice).csrf("x"), 403),
    ];
    table(&app, "CSRF (repeated)", rows).await;
}

#[tokio::test]
async fn csrf_the_token_of_an_ended_session_never_passes() {
    let (app, alice) = probed();
    assert_eq!(code(&app, session_post(&app, &alice)).await, 200);
    app.sessions.end(&alice.session).expect("end");
    let answer = app.send(session_post(&app, &alice)).await;
    assert_eq!(answer.status, 401);
    assert_eq!(answer.json(), json!({ "code": "invalid" }));
    // A new session has a new token.
    let again = app.sign_in("alice");
    assert_ne!(again.csrf, alice.csrf);
    assert_eq!(code(&app, app.change_as(Method::POST, "/t/session", &again)).await, 200);
}

#[tokio::test]
async fn csrf_every_changing_method_on_every_session_kind_needs_the_token() {
    let (app, alice) = probed();
    for path in ["/t/session", "/t/fresh"] {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let bare = app.change(method.clone(), path).cookie(&alice.cookie);
            assert_eq!(code(&app, bare).await, 403, "{method} {path}");
        }
    }
    let upload = app.change(Method::PUT, "/t/upload").set("content-type", "application/octet-stream").cookie(&alice.cookie);
    assert_eq!(code(&app, upload).await, 403, "upload");
}

#[tokio::test]
async fn csrf_a_public_part_and_a_read_need_no_token() {
    let (app, alice) = probed();
    assert_eq!(code(&app, app.change(Method::POST, "/t/public")).await, 200);
    assert_eq!(code(&app, app.change(Method::POST, "/t/public").cookie(&alice.cookie)).await, 200);
    assert_eq!(code(&app, app.req(Method::GET, "/t/session").cookie(&alice.cookie)).await, 200);
}

#[tokio::test]
async fn csrf_the_token_is_derived_per_session_and_not_from_the_cookie() {
    let (app, alice) = probed();
    let other = app.sign_in("alice");
    assert_eq!(alice.csrf.len(), 43);
    assert!(alice.csrf.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    assert_ne!(alice.csrf, alice.cookie);
    assert_ne!(alice.csrf, other.csrf);
    // Stable for one session: the page gets the same token on every call.
    let first = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await.json();
    app.clock.advance(40);
    let second = app.send(app.req(Method::GET, "/api/session").cookie(&alice.cookie)).await.json();
    assert_eq!(first["csrf"], second["csrf"]);
    assert_eq!(first["csrf"], json!(alice.csrf));
    // The token does not depend on the person: another account's session has another token.
    app.create_account("bob", Role::Member);
    assert_ne!(app.sign_in("bob").csrf, alice.csrf);
}

#[test]
fn csrf_the_comparison_is_constant_time_by_subtle() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/http/csrf.rs")).expect("the source");
    assert!(source.contains("subtle::ConstantTimeEq") && source.contains("ct_eq"));
    let token = "a".repeat(43);
    assert!(holzwave_server::http::csrf::token_ok(token.as_bytes(), &token));
    assert!(!holzwave_server::http::csrf::token_ok(&token.as_bytes()[..42], &token));
    assert!(!holzwave_server::http::csrf::token_ok(b"", &token));
}

// ---------------------------------------------------------------------------------------------------------------------
// Methods and the sharing headers
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn methods_options_is_405_without_any_sharing_header() {
    let (app, alice) = probed();
    for path in ["/t/public", "/t/session", "/api/session", "/nowhere", "/"] {
        let preflight = app
            .req(Method::OPTIONS, path)
            .header("origin", "https://evil.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "x-csrf-token")
            .cookie(&alice.cookie);
        let answer = app.send(preflight).await;
        assert_eq!(answer.status, 405, "{path}");
        assert_eq!(answer.json(), json!({ "code": "bad_request" }));
        assert!(answer.headers.keys().all(|n| !n.as_str().starts_with("access-control-")), "{path}");
        assert!(answer.headers.get("allow").is_none(), "{path}: {:?}", answer.headers);
    }
    println!("Methods: OPTIONS = 405 on 5 paths, TRACE and CONNECT = 405");
}

#[tokio::test]
async fn methods_trace_and_connect_are_405() {
    let (app, _) = probed();
    for method in [Method::TRACE, Method::CONNECT] {
        assert_eq!(code(&app, app.req(method.clone(), "/t/public")).await, 405, "{method}");
    }
    let custom = Method::from_bytes(b"PROPFIND").expect("a method");
    assert_eq!(code(&app, app.req(custom, "/t/public")).await, 405);
}

#[tokio::test]
async fn methods_a_wrong_method_and_an_unknown_path_have_the_plain_json_error() {
    let (app, _) = probed();
    let unknown = app.send(app.req(Method::GET, "/nowhere")).await;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json(), json!({ "code": "not_found" }));
    let wrong = app.send(app.change(Method::POST, "/api/session")).await;
    assert_eq!(wrong.status, 405);
    assert_eq!(wrong.json(), json!({ "code": "bad_request" }));
    assert_eq!(wrong.header("content-type").as_deref(), Some("application/json"));
}

#[tokio::test]
async fn methods_a_route_cannot_add_a_sharing_header() {
    let (app, _) = probed();
    let answer = app.send(app.req(Method::GET, "/t/cors").header("origin", "https://evil.example")).await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.text(), "ok");
    assert!(answer.headers.keys().all(|n| !n.as_str().starts_with("access-control-")), "{:?}", answer.header_names());
}

// ---------------------------------------------------------------------------------------------------------------------
// The proxy rule
// ---------------------------------------------------------------------------------------------------------------------

fn proxied() -> TestApp {
    TestApp::with(Options { proxy: true, parts: probe_parts(), ..Options::default() })
}

fn ip(text: &str) -> std::net::IpAddr {
    text.parse().expect("an address")
}

#[tokio::test]
async fn proxy_a_trusted_proxy_must_say_https_and_its_client_address_counts() {
    let app = proxied();
    let via = |proto: Option<&str>| {
        let req = app.req(Method::GET, "/t/public").peer(ip("10.1.2.3")).header("x-forwarded-for", "198.51.100.9");
        match proto {
            Some(proto) => req.header("x-forwarded-proto", proto),
            None => req,
        }
    };
    let good = app.send(via(Some("https"))).await;
    assert_eq!(good.status, 200);
    assert_eq!(good.json()["client"], json!("198.51.100.9"));
    let rows = vec![
        ("http".to_owned(), via(Some("http")), 400),
        ("missing".to_owned(), via(None), 400),
        ("twice".to_owned(), via(Some("https")).header("x-forwarded-proto", "https"), 400),
        ("a bad address list".to_owned(), via(Some("https")).set("x-forwarded-for", "not an address"), 400),
        ("https in other case".to_owned(), via(Some("HTTPS")), 200),
    ];
    table(&app, "Proxy", rows).await;
}

#[tokio::test]
async fn proxy_a_forwarded_header_from_anyone_else_is_ignored() {
    let app = proxied();
    let stranger = app
        .req(Method::GET, "/t/public")
        .peer(ip("203.0.113.5"))
        .header("x-forwarded-for", "198.51.100.9")
        .header("x-forwarded-proto", "http");
    let answer = app.send(stranger).await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["client"], json!("203.0.113.5"));
    // A forwarded host never replaces the Host.
    let spoof = app
        .req(Method::GET, "/t/public")
        .peer(ip("10.1.2.3"))
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-host", "hw.example.test")
        .set("host", "evil.example");
    assert_eq!(code(&app, spoof).await, 421);
}

#[tokio::test]
async fn proxy_without_the_proxy_mode_forwarded_headers_are_never_read() {
    let (app, _) = probed();
    let req = app.req(Method::GET, "/t/public").peer(ip("10.1.2.3")).header("x-forwarded-for", "198.51.100.9").header("x-forwarded-proto", "http");
    let answer = app.send(req).await;
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json()["client"], json!("10.1.2.3"));
}

// ---------------------------------------------------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------------------------------------------------

const VALUE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

fn headers_of(lines: &[&str]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for line in lines {
        map.append("cookie", HeaderValue::from_str(line).expect("a header"));
    }
    map
}

fn read(lines: &[&str]) -> Option<String> {
    cookies::read_session(&headers_of(lines))
}

#[test]
fn cookies_the_builders_give_exactly_the_contract_strings() {
    assert_eq!(VALUE.len(), 43);
    assert_eq!(cookies::session(VALUE).expect("a cookie"), format!("__Host-hw_session={VALUE}; Secure; HttpOnly; SameSite=Strict; Path=/"));
    assert_eq!(
        cookies::oidc("txn-1_a.b~c").expect("a cookie"),
        "__Host-hw_oidc=txn-1_a.b~c; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=600"
    );
    assert_eq!(
        cookies::pending("pend-1").expect("a cookie"),
        "__Host-hw_pending=pend-1; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=300"
    );
    println!("Cookies: session = {:?}", cookies::session(VALUE).expect("a cookie"));
    println!("Cookies: oidc = {:?}", cookies::oidc("v").expect("a cookie"));
    println!("Cookies: pending = {:?}", cookies::pending("v").expect("a cookie"));
}

#[test]
fn cookies_the_deleting_forms_empty_the_value_and_end_it() {
    assert_eq!(cookies::delete_session(), "__Host-hw_session=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    assert_eq!(cookies::delete_oidc(), "__Host-hw_oidc=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    assert_eq!(cookies::delete_pending(), "__Host-hw_pending=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
}

#[test]
fn cookies_a_browser_reads_the_attributes_the_contract_names() {
    use cookie::{Cookie, SameSite};
    let session = cookies::session(VALUE).expect("a cookie");
    let parsed = Cookie::parse(session.to_str().expect("text").to_owned()).expect("parses");
    assert_eq!(parsed.name(), "__Host-hw_session");
    assert_eq!(parsed.value(), VALUE);
    assert_eq!(parsed.secure(), Some(true));
    assert_eq!(parsed.http_only(), Some(true));
    assert_eq!(parsed.same_site(), Some(SameSite::Strict));
    assert_eq!(parsed.path(), Some("/"));
    assert!(parsed.domain().is_none() && parsed.max_age().is_none() && parsed.expires().is_none());

    let oidc = cookies::oidc("v").expect("a cookie");
    let parsed = Cookie::parse(oidc.to_str().expect("text").to_owned()).expect("parses");
    assert_eq!((parsed.secure(), parsed.http_only(), parsed.same_site(), parsed.path()), (Some(true), Some(true), Some(SameSite::Lax), Some("/")));
    assert_eq!(parsed.max_age(), Some(cookie::time::Duration::seconds(600)));
    assert!(parsed.domain().is_none());

    let pending = cookies::pending("v").expect("a cookie");
    let parsed = Cookie::parse(pending.to_str().expect("text").to_owned()).expect("parses");
    assert_eq!(parsed.same_site(), Some(SameSite::Strict));
    assert_eq!(parsed.max_age(), Some(cookie::time::Duration::seconds(300)));
    assert!(parsed.domain().is_none());

    for gone in [cookies::delete_session(), cookies::delete_oidc(), cookies::delete_pending()] {
        let parsed = Cookie::parse(gone.to_str().expect("text").to_owned()).expect("parses");
        assert_eq!(parsed.value(), "");
        assert_eq!(parsed.max_age(), Some(cookie::time::Duration::seconds(0)));
        assert_eq!((parsed.secure(), parsed.http_only(), parsed.path()), (Some(true), Some(true), Some("/")));
        assert!(parsed.domain().is_none());
    }
}

#[test]
fn cookies_a_builder_refuses_a_value_that_does_not_belong_in_a_cookie() {
    assert!(cookies::session(&VALUE[..42]).is_none());
    assert!(cookies::session(&format!("{VALUE}A")).is_none());
    assert!(cookies::session(&format!("{};", &VALUE[..42])).is_none());
    assert!(cookies::session("").is_none());
    for bad in ["", "a b", "a;b", "a,b", "a\"b", "a\r\nSet-Cookie: x=y", "a=b", &"x".repeat(257)] {
        assert!(cookies::oidc(bad).is_none(), "{bad:?}");
        assert!(cookies::pending(bad).is_none(), "{bad:?}");
    }
}

#[test]
fn cookies_the_reader_finds_the_value_among_other_cookies() {
    let line = format!("theme=dark; __Host-hw_session={VALUE}; lang=de");
    assert_eq!(read(&[&line]).as_deref(), Some(VALUE));
    assert_eq!(read(&[&format!("__Host-hw_session={VALUE}")]).as_deref(), Some(VALUE));
    assert_eq!(read(&["a=1", &format!("__Host-hw_session={VALUE}")]).as_deref(), Some(VALUE), "two header lines");
    assert_eq!(read(&["theme=dark"]), None);
    assert_eq!(read(&[]), None);
    println!("Cookies: reader finds the value among other cookies and in a second header line");
}

#[test]
fn cookies_the_reader_refuses_a_repeated_name() {
    let a = format!("__Host-hw_session={VALUE}");
    let b = format!("__Host-hw_session={}", VALUE.to_uppercase());
    assert_eq!(read(&[&format!("{a}; {a}")]), None);
    assert_eq!(read(&[&format!("{a}; {b}")]), None);
    assert_eq!(read(&[&a, &b]), None, "in two header lines");
    assert_eq!(read(&[&format!("{b}; x=1; {a}")]), None);
}

#[test]
fn cookies_the_reader_refuses_values_of_the_wrong_shape() {
    let cases: Vec<(&str, String)> = vec![
        ("42 characters", VALUE[..42].to_owned()),
        ("44 characters", format!("{VALUE}A")),
        ("quoted", format!("\"{VALUE}\"")),
        ("space inside", format!("{} {}", &VALUE[..20], &VALUE[21..])),
        ("padding", format!("{}=", &VALUE[..42])),
        ("plus", format!("{}+", &VALUE[..42])),
        ("slash", format!("{}/", &VALUE[..42])),
        ("percent", format!("{}%41", &VALUE[..40])),
        ("empty", String::new()),
    ];
    for (name, value) in cases {
        assert_eq!(read(&[&format!("__Host-hw_session={value}")]), None, "{name}");
    }
    assert_eq!(read(&[&format!("__host-hw_session={VALUE}")]), None, "the name is case-sensitive");
    assert_eq!(read(&[&format!("hw_session={VALUE}")]), None, "without the prefix");
    assert_eq!(read(&[&format!("x__Host-hw_session={VALUE}")]), None);
    println!("Cookies: reader refuses 42, 44, quoted, space inside, padding, plus, slash, percent, empty, other case, no prefix");
}

#[test]
fn cookies_the_reader_refuses_a_malformed_pair_anywhere_in_the_header() {
    let ok = format!("__Host-hw_session={VALUE}");
    assert_eq!(read(&[&format!("novalue; {ok}")]), None);
    assert_eq!(read(&[&format!("=x; {ok}")]), None);
    assert_eq!(read(&[&format!("{ok}; =")]), None);
}

#[test]
fn cookies_the_reader_is_bounded() {
    let ok = format!("__Host-hw_session={VALUE}");
    // 64 pairs pass, 65 do not.
    let pairs = |n: usize| {
        let mut parts: Vec<String> = (0..n - 1).map(|i| format!("c{i}=1")).collect();
        parts.push(ok.clone());
        parts.join("; ")
    };
    assert_eq!(read(&[&pairs(64)]).as_deref(), Some(VALUE));
    assert_eq!(read(&[&pairs(65)]), None);
    // 8 KiB passes, 8 KiB plus one byte does not.
    let padded = |len: usize| {
        let filler = len - ok.len() - "; p=".len();
        format!("{ok}; p={}", "x".repeat(filler))
    };
    let at = padded(8 * 1024);
    assert_eq!(at.len(), 8 * 1024);
    assert_eq!(read(&[&at]).as_deref(), Some(VALUE));
    assert_eq!(read(&[&padded(8 * 1024 + 1)]), None);
    // The bound is on all lines together.
    let half = format!("p={}", "x".repeat(4_500));
    assert_eq!(read(&[&half, &format!("{ok}; {half}")]), None);
    println!("Cookies: reader bounds = 64 pairs pass, 65 do not; 8192 bytes pass, 8193 do not");
}
