//! The sign-in routes through the real router: the password and code flow, the passkey and single sign-on flows, the method
//! matrix, logout, unlock and step-up, the throttle through the routes, hostile input, overload and the canary.
//!
//! The router is called as a `tower` service (no socket). The services behind it are the real ones over a scratch store and a
//! manual clock; the secret store is a recording fake of the unlock gate. See `support/auth_env.rs`.

#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/edge.rs"]
mod edge;

use auth_env::{
    AuthEnv, FIREFOX, PASSWORD, WRONG, WithCookies, fingerprint, set_cookie_line, set_cookie_value,
};
use edge::Resp;
use holzwave_server::auth::session::Method as SignInMethod;
use holzwave_server::clock::Clock as _;
use holzwave_server::http::routes_auth::device_of;
use holzwave_server::store::AccountState;
use serde_json::{Value, json};

const PENDING: &str = "__Host-hw_pending";
const SESSION: &str = "__Host-hw_session";

const PENDING_TAIL: &str = "; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=300";
const SESSION_TAIL: &str = "; Secure; HttpOnly; SameSite=Strict; Path=/";
const PENDING_GONE: &str = "__Host-hw_pending=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0";

/// The answer every failed sign-in step gives: 401 `invalid`, no cookie, no `Retry-After`.
fn assert_invalid(response: &Resp, what: &str) {
    assert_eq!(response.status, 401, "{what}: {}", response.text());
    assert_eq!(response.json(), json!({"code": "invalid"}), "{what}");
    assert!(response.all("set-cookie").is_empty(), "{what}: a cookie was set");
    assert!(response.header("retry-after").is_none(), "{what}: a retry-after header");
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
}

fn token_of(line: &str, name: &str, tail: &str) -> String {
    let value = line.strip_prefix(&format!("{name}=")).and_then(|rest| rest.strip_suffix(tail)).unwrap_or_else(|| panic!("not the exact cookie line: {line}"));
    assert_eq!(value.len(), 43, "the value is 43 characters");
    value.to_owned()
}

// ---- the tracer ----

#[tokio::test]
async fn tracer_a_password_and_a_code_make_one_session() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;

    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(first.status, 200, "{}", first.text());
    assert_eq!(first.json(), json!({"next": "totp"}));
    assert_eq!(first.header("cache-control").as_deref(), Some("no-store"));
    let pending_line = set_cookie_line(&first, PENDING).expect("a pending cookie");
    let pending = token_of(&pending_line, PENDING, PENDING_TAIL);
    assert_eq!(first.all("set-cookie").len(), 1, "only the pending cookie");
    assert!(set_cookie_value(&first, SESSION).is_none(), "no session after the first step");
    assert_eq!(env.session_rows(alice), 0);
    println!("Tracer: first step 200 {} set-cookie [{}…{}] cache-control no-store", first.text(), &pending_line[..PENDING.len() + 1], PENDING_TAIL);

    let me = env.send(env.get("/api/session").set_cookies(&[(PENDING, &pending)])).await;
    assert_eq!(me.json()["state"], "anonymous", "the pending cookie is no session");

    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &pending)]))
        .await;
    assert_eq!(second.status, 200, "{}", second.text());
    assert_eq!(second.json(), json!({"next": "done"}));
    assert_eq!(second.header("cache-control").as_deref(), Some("no-store"));
    let session_line = set_cookie_line(&second, SESSION).expect("a session cookie");
    let session = token_of(&session_line, SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&second, PENDING).as_deref(), Some(PENDING_GONE), "the pending cookie is deleted");
    assert_eq!(second.all("set-cookie").len(), 2);
    assert!(!second.text().contains(&session), "the body carries no session value");
    assert!(!second.text().contains("csrf") && !second.text().contains("alice"), "no token and no account fact");
    println!("Tracer: second step 200 {} set-cookie [{SESSION}=<43>{SESSION_TAIL}] [{PENDING_GONE}]", second.text());

    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &session)])).await;
    assert_eq!(me.json()["state"], "signed_in");
    assert_eq!(me.json()["account"]["username"], "alice");
    assert_eq!(env.session_rows(alice), 1);
    let row = env.sessions.lookup(&session, env.clock.unix()).expect("alive");
    assert_eq!(row.method, SignInMethod::PasswordTotp);
    assert_eq!(row.device, "Firefox on Linux");

    let calls = env.gate.started();
    assert_eq!(calls.len(), 1, "the gate was told once");
    assert_eq!((calls[0].account, calls[0].kind, calls[0].key.len()), (alice, "password", 32));
    println!("Gate: account={} kind={} key_len={}", calls[0].account.get(), calls[0].kind, calls[0].key.len());
    let ok: Vec<Value> = env.audit_lines().into_iter().filter(|l| l["action"] == "signin.ok").collect();
    assert_eq!(ok.len(), 1);
}

#[tokio::test]
async fn tracer_every_failed_step_looks_the_same() {
    let env = AuthEnv::all();
    env.person("alice").await;
    env.person("dora").await;
    env.set_state("dora", AccountState::Disabled);
    let bob = env.bare("bob", holzwave_server::store::Role::Member);
    env.set_password(bob).await; // a password and no authenticator

    let mut rows: Vec<(&str, Resp)> = Vec::new();
    let password = |name: &str, password: &str| env.post("/auth/password", &json!({"username": name, "password": password}));
    rows.push(("wrong password", env.send(password("alice", WRONG)).await));
    rows.push(("unknown user", env.send(password("nobody", PASSWORD)).await));
    rows.push(("disabled account", env.send(password("dora", PASSWORD)).await));
    rows.push(("no authenticator", env.send(password("bob", PASSWORD)).await));
    rows.push(("empty password", env.send(password("alice", "")).await));

    // The second step: a real pending state, a wrong code and then the same code twice.
    let first = env.send(password("alice", PASSWORD)).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let totp = |code: &str, cookie: Option<&str>| {
        let req = env.post("/auth/totp", &json!({"code": code}));
        match cookie {
            Some(value) => req.set_cookies(&[(PENDING, value)]),
            None => req,
        }
    };
    rows.push(("wrong code", env.send(totp(&env.wrong_code("alice"), Some(&pending))).await));
    let good = env.send(totp(&env.code("alice"), Some(&pending))).await;
    assert_eq!(good.status, 200, "the right code still works after one wrong one");
    let again = env.send(password("alice", PASSWORD)).await;
    let pending_again = set_cookie_value(&again, PENDING).expect("pending");
    rows.push(("code used twice", env.send(totp(&env.code("alice"), Some(&pending_again))).await));
    rows.push(("no pending cookie", env.send(totp(&env.code("alice"), None)).await));
    rows.push(("pending cookie never issued", env.send(totp(&env.code("alice"), Some(&"A".repeat(43)))).await));
    rows.push(("pending cookie of 44 characters", env.send(totp(&env.code("alice"), Some(&"A".repeat(44)))).await));

    let reference = fingerprint(&rows[0].1);
    for (what, response) in &rows {
        assert_invalid(response, what);
        assert_eq!(fingerprint(response), reference, "{what} differs from the first row byte for byte");
    }
    println!("Sameness: {} failure rows, all equal: {}", rows.len(), rows.iter().map(|r| r.0).collect::<Vec<_>>().join(", "));
    println!("Sameness: status 401, body {}, headers {:?}", rows[0].1.text(), rows[0].1.header_names());
}

#[tokio::test]
async fn tracer_a_new_sign_in_ends_the_session_the_browser_held() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let old = env.password_sign_in("alice").await;
    assert_eq!(env.session_rows(alice), 1);

    // The browser still holds the old cookie and signs in again; the code of the next step is needed.
    env.clock.advance(31);
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(SESSION, &old.cookie), (PENDING, &pending)]))
        .await;
    assert_eq!(second.status, 200, "{}", second.text());
    let new = set_cookie_value(&second, SESSION).expect("a session cookie");
    assert_ne!(new, old.cookie, "a new value at every sign-in");
    assert!(env.sessions.lookup(&old.cookie, env.clock.unix()).is_err(), "the old session is gone");
    assert!(env.sessions.lookup(&new, env.clock.unix()).is_ok());
    assert_eq!(env.session_rows(alice), 1, "no orphan");
}

#[tokio::test]
async fn tracer_the_cookies_have_the_exact_attributes() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending_line = set_cookie_line(&first, PENDING).expect("pending");
    let pending = token_of(&pending_line, PENDING, PENDING_TAIL);
    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &pending)]))
        .await;
    let session_line = set_cookie_line(&second, SESSION).expect("session");
    token_of(&session_line, SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&second, PENDING).as_deref(), Some(PENDING_GONE));
    for line in [&pending_line, &session_line] {
        assert!(line.starts_with("__Host-"), "the prefix");
        assert!(!line.contains("Domain"), "no domain");
    }
    assert!(!session_line.contains("Max-Age"), "a session cookie has no Max-Age");
}

// ---- the password route ----

#[tokio::test]
async fn the_password_route_refuses_what_is_not_exactly_its_body() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let cases: Vec<(&str, Value)> = vec![
        ("an unknown member", json!({"username": "alice", "password": PASSWORD, "extra": 1})),
        ("a missing password", json!({"username": "alice"})),
        ("a number as the name", json!({"username": 5, "password": PASSWORD})),
        ("null values", json!({"username": null, "password": null})),
        ("an array", json!(["alice", PASSWORD])),
    ];
    for (what, body) in cases {
        let response = env.send(env.post("/auth/password", &body)).await;
        assert_eq!(response.status, 400, "{what}: {}", response.text());
        assert_eq!(response.json(), json!({"code": "bad_request"}), "{what}");
        assert!(response.all("set-cookie").is_empty(), "{what}");
    }
}

#[tokio::test]
async fn the_password_route_keeps_the_origin_and_content_type_checks() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let body = json!({"username": "alice", "password": PASSWORD});
    let no_origin = env.send(env.post("/auth/password", &body).without("origin")).await;
    assert_eq!(no_origin.status, 403);
    let foreign = env.send(env.post("/auth/password", &body).set("origin", "https://evil.example")).await;
    assert_eq!(foreign.status, 403);
    let text = env.send(env.post("/auth/password", &body).set("content-type", "text/plain")).await;
    assert_eq!(text.status, 400);
    assert_eq!(env.gate.started().len(), 0);
    assert_eq!(env.argon.derivations(), 0, "none of the three reached a derivation");
}

#[tokio::test]
async fn the_password_route_needs_no_session_and_no_token() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let response = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(response.status, 200, "no session cookie and no X-CSRF-Token needed");
}

#[tokio::test]
async fn a_wrong_password_answers_429_with_the_seconds_once_the_name_is_locked() {
    let env = AuthEnv::all();
    env.person("alice").await;
    for n in 1..=5 {
        let response = env.send(env.post("/auth/password", &json!({"username": "alice", "password": WRONG}))).await;
        assert_invalid(&response, &format!("failure {n}"));
    }
    let locked = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    let body = locked.json();
    assert_eq!(body["code"], "throttled");
    let seconds = body["retry_seconds"].as_u64().expect("seconds");
    assert!((1..=900).contains(&seconds));
    assert_eq!(locked.header("retry-after").as_deref(), Some(seconds.to_string().as_str()));
    assert!(locked.all("set-cookie").is_empty());
    // An unknown name is locked in the same way: the same status, body shape and seconds.
    for _ in 1..=5 {
        env.send(env.post("/auth/password", &json!({"username": "nobody", "password": WRONG}))).await;
    }
    let unknown = env.send(env.post("/auth/password", &json!({"username": "nobody", "password": PASSWORD}))).await;
    assert_eq!(unknown.status, 429);
    assert_eq!(unknown.json()["code"], "throttled");
}

#[tokio::test]
async fn a_gate_that_fails_does_not_fail_the_sign_in() {
    let env = AuthEnv::all();
    env.person("alice").await;
    env.gate.fail_started.store(true, std::sync::atomic::Ordering::SeqCst);
    let signed = env.password_sign_in("alice").await;
    assert_eq!(env.gate.started().len(), 1, "the gate was asked");
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &signed.cookie)])).await;
    assert_eq!(me.json()["state"], "signed_in");
}

#[tokio::test]
async fn the_password_key_is_not_in_any_answer_or_audit_line() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &pending)]))
        .await;
    let key = env.gate.started()[0].key.clone();
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &key);
    let audit = env.audit_text();
    for (what, text) in [("first", first.text()), ("second", second.text()), ("audit", audit)] {
        assert!(!text.contains(&hex) && !text.contains(&b64) && !text.contains(PASSWORD), "{what} leaks");
    }
}

// ---- the device summary ----

const CHROME_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const EDGE_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.2592.87";
const SAFARI_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const SAFARI_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const CHROME_ANDROID: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36";

#[test]
fn the_device_summary_comes_from_a_closed_list() {
    let table = [
        (FIREFOX, "Firefox on Linux"),
        (CHROME_WINDOWS, "Chrome on Windows"),
        (EDGE_WINDOWS, "Edge on Windows"),
        (SAFARI_MAC, "Safari on macOS"),
        (SAFARI_IPHONE, "Safari on iOS"),
        (CHROME_ANDROID, "Chrome on Android"),
        ("", "Browser"),
        ("curl/8.5.0", "Browser"),
    ];
    for (agent, expected) in table {
        assert_eq!(device_of(agent), expected, "for {agent:?}");
    }
    println!("Device: {}", table.iter().map(|(_, d)| *d).collect::<Vec<_>>().join(" | "));
}

#[test]
fn the_device_summary_is_bounded_and_never_the_raw_header() {
    let huge = "A".repeat(10 * 1024);
    assert_eq!(device_of(&huge), "Browser");
    assert_eq!(device_of(&format!("{FIREFOX} {huge}")), "Browser", "a header of 10 KiB is not looked at");
    for agent in [FIREFOX, CHROME_WINDOWS, "\u{1F980}\u{1F980}\u{1F980} Firefox/1 Linux", "Firefox/1 \u{0}\u{7}\r\n", &huge, ""] {
        let device = device_of(agent);
        assert!(device.len() <= 64, "{device:?}");
        assert!(device.bytes().all(|b| (0x20..0x7f).contains(&b)), "{device:?} is printable ASCII");
        assert_ne!(device, agent, "never the raw header");
    }
}

#[tokio::test]
async fn the_session_records_the_device_not_the_header() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD})).set("user-agent", EDGE_WINDOWS)).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set("user-agent", EDGE_WINDOWS).set_cookies(&[(PENDING, &pending)]))
        .await;
    let signed = env.signed_from(&second);
    assert_eq!(signed.session.device, "Edge on Windows");
    assert_eq!(signed.session.addr, "0.0.0.0", "the client address comes from the application layer");
}
