//! The HTTP application layer: who the caller is, whether the request may change anything, whether it came to the right name,
//! and what every response says about itself.

#[path = "support/edge.rs"]
mod edge;

use axum::http::Method;
use edge::{ABSOLUTE, HOST, IDLE, NOW, ORIGIN, STEP_UP, TestApp};
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
