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
    let before = env.argon.derivations();
    let body = json!({"username": "alice", "password": PASSWORD});
    let no_origin = env.send(env.post("/auth/password", &body).without("origin")).await;
    assert_eq!(no_origin.status, 403);
    let foreign = env.send(env.post("/auth/password", &body).set("origin", "https://evil.example")).await;
    assert_eq!(foreign.status, 403);
    let text = env.send(env.post("/auth/password", &body).set("content-type", "text/plain")).await;
    assert_eq!(text.status, 400);
    assert_eq!(env.gate.started().len(), 0);
    assert_eq!(env.argon.derivations(), before, "none of the three reached a derivation");
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

// ---------------------------------------------------------------------------------------------------------------------
// Passkeys
// ---------------------------------------------------------------------------------------------------------------------

const PENDING_GONE_LINE: &str = PENDING_GONE;
const OIDC: &str = "__Host-hw_oidc";
const OIDC_TAIL: &str = "; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=600";
const OIDC_GONE: &str = "__Host-hw_oidc=; Secure; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";

/// The finish request of a passkey sign-in with the given pending cookie, credential and `prf` member.
async fn finish_passkey(env: &AuthEnv, pending: Option<&str>, credential: Value, prf: Option<Value>) -> Resp {
    let mut body = json!({ "credential": credential });
    if let Some(prf) = prf {
        body["prf"] = prf;
    }
    let req = env.post("/auth/passkey/finish", &body);
    let req = match pending {
        Some(value) => req.set_cookies(&[(PENDING, value)]),
        None => req,
    };
    env.send(req).await
}

#[tokio::test]
async fn passkey_begin_gives_the_options_and_the_pending_cookie() {
    let env = AuthEnv::all();
    let (response, options, pending) = env.passkey_begin().await;
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    let body = response.json();
    assert_eq!(body.as_object().expect("object").keys().collect::<Vec<_>>(), ["options"], "nothing but the options");
    let public_key = options["publicKey"].as_object().expect("publicKey");
    assert_eq!(public_key["rpId"], "hw.example.test");
    assert_eq!(public_key["allowCredentials"], json!([]));
    assert_eq!(public_key["userVerification"], "required");
    assert!(public_key["extensions"]["prf"]["eval"]["first"].is_string());
    let line = set_cookie_line(&response, PENDING).expect("pending cookie");
    assert_eq!(token_of(&line, PENDING, PENDING_TAIL), pending);
    assert_eq!(response.all("set-cookie").len(), 1);
    println!("Passkey: options keys {:?}", public_key.keys().collect::<Vec<_>>());
}

#[tokio::test]
async fn a_passkey_with_prf_signs_in_and_tells_the_gate_the_material() {
    let env = AuthEnv::all();
    let (alice, mut key) = env.passkey_person("alice");
    let response = env.passkey_sign_in(&mut key, Some(&auth_env::prf_text(9))).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json(), json!({"next": "done"}));
    let session_line = set_cookie_line(&response, SESSION).expect("session cookie");
    token_of(&session_line, SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&response, PENDING).as_deref(), Some(PENDING_GONE_LINE));
    assert_eq!(response.all("set-cookie").len(), 2);
    let signed = env.signed_from(&response);
    assert_eq!(signed.session.method, SignInMethod::Passkey);
    assert_eq!(signed.account, alice);
    let calls = env.gate.started();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].account, calls[0].kind), (alice, "passkey"));
    assert_eq!(calls[0].key, vec![9u8; 32], "the 32 bytes the page sent");
    let ok: Vec<Value> = env.audit_lines().into_iter().filter(|l| l["action"] == "signin.ok").collect();
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0]["detail"]["method"], "passkey");
    println!("Passkey: success 200 {} gate kind={} key_len={}", response.text(), calls[0].kind, calls[0].key.len());
}

#[tokio::test]
async fn a_passkey_without_prf_signs_in_and_the_gate_gets_no_material() {
    let env = AuthEnv::all();
    let (_, mut key) = env.passkey_person("alice");
    let response = env.passkey_sign_in(&mut key, None).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let calls = env.gate.started();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].kind, calls[0].key.len()), ("none", 0));
}

#[tokio::test]
async fn a_prf_that_is_not_32_bytes_signs_in_without_material() {
    let env = AuthEnv::all();
    let (_, mut key) = env.passkey_person("alice");
    for (what, prf) in [
        ("31 bytes", base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, [1u8; 31])),
        ("longer than the bound", "A".repeat(65)),
        ("not base64url", "!".repeat(43)),
    ] {
        env.clock.advance(1);
        let response = env.passkey_sign_in(&mut key, Some(&prf)).await;
        assert_eq!(response.status, 200, "{what}: {}", response.text());
    }
    let calls = env.gate.started();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|c| c.kind == "none" && c.key.is_empty()), "no material for a bad prf");
}

#[tokio::test]
async fn every_failed_passkey_sign_in_looks_the_same() {
    let env = AuthEnv::all();
    let (_, mut alice) = env.passkey_person("alice");
    let (_, mut dora) = env.passkey_person("dora");
    let stranger_env = AuthEnv::all();
    let (_, mut stranger) = stranger_env.passkey_person("alice");
    env.set_state("dora", AccountState::Disabled);
    let other_site = webauthn_rs::prelude::Url::parse("https://other.example.test").expect("url");
    let mut rows: Vec<(&str, Resp)> = Vec::new();

    let (_, options, pending) = env.passkey_begin().await;
    let credential = alice.authenticate(&options, &env.origin).expect("answers");
    assert_eq!(finish_passkey(&env, Some(&pending), credential.clone(), None).await.status, 200);
    rows.push(("the same response sent twice", finish_passkey(&env, Some(&pending), credential, None).await));

    let (_, options, pending) = env.passkey_begin().await;
    let credential = alice.authenticate_with(&options, &other_site, &auth_env::webauthn_client::Tweaks::unchecked()).expect("answers");
    rows.push(("a response for another origin", finish_passkey(&env, Some(&pending), credential, None).await));

    let (_, options, pending) = env.passkey_begin().await;
    let credential = stranger.authenticate(&options, &env.origin).expect("answers");
    rows.push(("an unknown authenticator", finish_passkey(&env, Some(&pending), credential, None).await));

    let (_, options, pending) = env.passkey_begin().await;
    let credential = dora.authenticate(&options, &env.origin).expect("answers");
    rows.push(("a disabled account", finish_passkey(&env, Some(&pending), credential, None).await));

    let (_, options, _) = env.passkey_begin().await;
    let credential = alice.authenticate(&options, &env.origin).expect("answers");
    rows.push(("no pending cookie", finish_passkey(&env, None, credential.clone(), None).await));
    rows.push(("a pending cookie never issued", finish_passkey(&env, Some(&"B".repeat(43)), credential, None).await));

    let (_, _, pending) = env.passkey_begin().await;
    rows.push(("a credential that is not an object", finish_passkey(&env, Some(&pending), json!("nope"), None).await));

    let (_, _, pending) = env.passkey_begin().await;
    let before = env.passkeys.library_calls();
    let huge = json!({"id": "A".repeat(40 * 1024), "type": "public-key"});
    rows.push(("a credential of 40 KiB", finish_passkey(&env, Some(&pending), huge, None).await));
    assert_eq!(env.passkeys.library_calls(), before, "the library was not reached by the 40 KiB credential");

    let reference = fingerprint(&rows[0].1);
    for (what, response) in &rows {
        assert_invalid(response, what);
        assert_eq!(fingerprint(response), reference, "{what} differs byte for byte");
    }
    println!("Passkey: {} failure rows, all equal: {}", rows.len(), rows.iter().map(|r| r.0).collect::<Vec<_>>().join(", "));
}

#[tokio::test]
async fn a_passkey_response_makes_one_session_when_sent_twice() {
    let env = AuthEnv::all();
    let (alice, mut key) = env.passkey_person("alice");
    let (_, options, pending) = env.passkey_begin().await;
    let credential = key.authenticate(&options, &env.origin).expect("answers");
    let first = finish_passkey(&env, Some(&pending), credential.clone(), None).await;
    let second = finish_passkey(&env, Some(&pending), credential, None).await;
    assert_eq!((first.status.as_u16(), second.status.as_u16()), (200, 401));
    assert_eq!(env.session_rows(alice), 1);
}

#[tokio::test]
async fn the_challenge_of_one_begin_does_not_open_the_cookie_of_another() {
    let env = AuthEnv::all();
    let (alice, mut key) = env.passkey_person("alice");
    let (_, options_a, pending_a) = env.passkey_begin().await;
    let (_, _options_b, pending_b) = env.passkey_begin().await;
    assert_ne!(pending_a, pending_b);
    let credential = key.authenticate(&options_a, &env.origin).expect("answers");
    let crossed = finish_passkey(&env, Some(&pending_b), credential.clone(), None).await;
    assert_invalid(&crossed, "options of one begin, cookie of the other");
    assert_eq!(env.session_rows(alice), 0);
}

#[tokio::test]
async fn a_passkey_sign_in_ends_the_session_the_browser_held() {
    let env = AuthEnv::all();
    env.person("bob").await;
    let old = env.password_sign_in("bob").await;
    let (alice, mut key) = env.passkey_person("alice");
    let (_, options, pending) = env.passkey_begin().await;
    let credential = key.authenticate(&options, &env.origin).expect("answers");
    let body = json!({"credential": credential});
    let response = env.send(env.post("/auth/passkey/finish", &body).set_cookies(&[(SESSION, &old.cookie), (PENDING, &pending)])).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert!(env.sessions.lookup(&old.cookie, env.clock.unix()).is_err(), "the held session was ended");
    assert_eq!(env.session_rows(old.account), 0);
    assert_eq!(env.session_rows(alice), 1);
}

#[tokio::test]
async fn the_passkey_routes_refuse_what_is_not_exactly_their_body() {
    let env = AuthEnv::all();
    let (_, mut key) = env.passkey_person("alice");
    let (_, options, pending) = env.passkey_begin().await;
    let credential = key.authenticate(&options, &env.origin).expect("answers");
    let cases: Vec<(&str, Value)> = vec![
        ("an unknown member", json!({"credential": credential, "extra": 1})),
        ("no credential", json!({"prf": "x"})),
        ("a number as the prf", json!({"credential": credential, "prf": 5})),
        ("an array", json!([credential])),
        ("null", json!(null)),
    ];
    for (what, body) in cases {
        let response = env.send(env.post("/auth/passkey/finish", &body).set_cookies(&[(PENDING, &pending)])).await;
        assert_eq!(response.status, 400, "{what}: {}", response.text());
        assert_eq!(response.json(), json!({"code": "bad_request"}), "{what}");
    }
    let begin = env.send(env.post("/auth/passkey/begin", &json!({"extra": 1}))).await;
    assert_eq!(begin.status, 400, "begin takes an empty object");
    // The pending cookie of the refused bodies is still unused: the right body works.
    let ok = finish_passkey(&env, Some(&pending), credential, None).await;
    assert_eq!(ok.status, 200, "{}", ok.text());
}

#[tokio::test]
async fn the_passkey_routes_keep_the_origin_and_content_type_checks() {
    let env = AuthEnv::all();
    let no_origin = env.send(env.post("/auth/passkey/begin", &json!({})).without("origin")).await;
    assert_eq!(no_origin.status, 403);
    let text = env.send(env.post("/auth/passkey/begin", &json!({})).set("content-type", "text/plain")).await;
    assert_eq!(text.status, 400);
    let get = env.send(env.get("/auth/passkey/begin")).await;
    assert_eq!(get.status, 405);
    assert_eq!(env.passkeys.open_challenges(), 0, "none of them opened a challenge");
}

#[tokio::test]
async fn a_passkey_sign_in_writes_one_line_and_a_failure_one_line() {
    let env = AuthEnv::all();
    let (_, mut key) = env.passkey_person("alice");
    env.passkey_sign_in(&mut key, None).await;
    let (_, _, pending) = env.passkey_begin().await;
    finish_passkey(&env, Some(&pending), json!("nope"), None).await;
    assert_eq!(env.count("signin.ok"), 1);
    assert_eq!(env.count("signin.fail"), 1);
    let fail = env.audit_lines().into_iter().find(|l| l["action"] == "signin.fail").expect("a failure line");
    assert_eq!(fail["detail"]["method"], "passkey");
    assert_eq!(fail["actor"], "anonymous");
}

// ---------------------------------------------------------------------------------------------------------------------
// Single sign-on
// ---------------------------------------------------------------------------------------------------------------------

use auth_env::oidc_provider::TestProvider;

/// A callback or start that failed: the fixed redirect, the transaction cookie deleted, nothing else.
fn assert_oidc_failure(response: &Resp, what: &str) {
    assert_eq!(response.status, 302, "{what}: {}", response.text());
    assert_eq!(response.header("location").as_deref(), Some("/?error=oidc"), "{what}");
    assert_eq!(response.all("set-cookie"), vec![OIDC_GONE.to_owned()], "{what}: only the transaction cookie, deleted");
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
}

#[tokio::test]
async fn oidc_start_redirects_to_the_provider_and_sets_the_lax_cookie() {
    let env = AuthEnv::all();
    let start = env.send(env.get("/auth/oidc/corp/start")).await;
    assert_eq!(start.status, 302, "{}", start.text());
    let location = start.header("location").expect("location");
    assert!(location.starts_with("https://idp.example.test/authorize?"), "{location}");
    assert_eq!(TestProvider::query_value(&location, "code_challenge_method").as_deref(), Some("S256"));
    assert!(TestProvider::query_value(&location, "state").is_some() && TestProvider::query_value(&location, "nonce").is_some());
    let line = set_cookie_line(&start, OIDC).expect("transaction cookie");
    token_of(&line, OIDC, OIDC_TAIL);
    assert_eq!(start.all("set-cookie").len(), 1);
    assert_eq!(start.header("cache-control").as_deref(), Some("no-store"));
    println!("OIDC: start 302 location=https://idp.example.test/authorize?… code_challenge_method=S256 set-cookie [{OIDC}=<43>{OIDC_TAIL}]");
}

#[tokio::test]
async fn two_starts_make_two_transactions() {
    let env = AuthEnv::all();
    let (_, first_callback, first_cookie) = env.oidc_begin().await;
    let (_, second_callback, second_cookie) = env.oidc_begin().await;
    assert_ne!(first_cookie, second_cookie);
    assert_ne!(first_callback.state, second_callback.state);
    // The cookie of one transaction does not finish the other.
    let crossed = env.send(env.oidc_callback_request(&first_callback.code, &first_callback.state, Some(&second_cookie))).await;
    assert_oidc_failure(&crossed, "state of one, cookie of the other");
}

#[tokio::test]
async fn the_oidc_callback_answers_a_success_with_the_login_page() {
    let env = AuthEnv::all();
    let (_, callback, transaction) = env.oidc_begin().await;
    let response = env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.text(), auth_env::LOGIN_PAGE);
    assert!(response.header("content-type").expect("content type").starts_with("text/html"));
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    assert!(response.header("location").is_none());
    let session_line = set_cookie_line(&response, SESSION).expect("session cookie");
    token_of(&session_line, SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&response, OIDC).as_deref(), Some(OIDC_GONE));
    assert_eq!(response.all("set-cookie").len(), 2);
    let signed = env.signed_from(&response);
    assert_eq!(signed.session.method, SignInMethod::Oidc);
    let calls = env.gate.started();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].kind, calls[0].key.len()), ("none", 0));
    assert_eq!(env.count("signin.ok"), 1);
    println!("OIDC: success 200 text/html no-store, body = the login page, session cookie Strict, transaction cookie deleted, gate kind none");
}

#[tokio::test]
async fn the_oidc_callback_page_is_not_changed_by_the_query() {
    let env = AuthEnv::all();
    let (_, callback, transaction) = env.oidc_begin().await;
    let hostile = "&next=%2F%2Fevil.example&x=%3Cscript%3Ealert(1)%3C%2Fscript%3E&state2=1";
    let request = env.get(&format!("/auth/oidc/callback?code={}&state={}{hostile}", callback.code, callback.state));
    let response = env.send(request.set_cookies(&[(OIDC, &transaction)])).await;
    assert_eq!(response.status, 200, "unknown parameters are ignored: {}", response.text());
    assert_eq!(response.text(), auth_env::LOGIN_PAGE, "the page is the page, whatever the query says");
}

#[tokio::test]
async fn every_oidc_callback_failure_is_the_same_redirect() {
    let env = AuthEnv::all();
    let mut rows: Vec<(&str, Resp)> = Vec::new();

    let (_, callback, transaction) = env.oidc_begin().await;
    rows.push(("a wrong state", env.send(env.oidc_callback_request(&callback.code, "wrong-state", Some(&transaction))).await));

    let (_, callback, _) = env.oidc_begin().await;
    rows.push(("no transaction cookie", env.send(env.oidc_callback_request(&callback.code, &callback.state, None)).await));

    let (_, callback, _) = env.oidc_begin().await;
    rows.push(("a cookie never issued", env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&"A".repeat(43)))).await));

    let (_, callback, transaction) = env.oidc_begin().await;
    let ok = env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await;
    assert_eq!(ok.status, 200);
    rows.push(("a replayed callback", env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await));

    let (_, callback, transaction) = env.oidc_begin().await;
    let request = env.get(&format!("/auth/oidc/callback?error=access_denied&state={}", callback.state));
    rows.push(("an error from the provider", env.send(request.set_cookies(&[(OIDC, &transaction)])).await));

    let (_, callback, transaction) = env.oidc_begin().await;
    let big = "a".repeat(3 * 1024);
    rows.push(("a code of 3 KiB", env.send(env.oidc_callback_request(&big, &callback.state, Some(&transaction))).await));

    let (_, callback, transaction) = env.oidc_begin().await;
    let request = env.get(&format!("/auth/oidc/callback?state={}", callback.state));
    rows.push(("no code", env.send(request.set_cookies(&[(OIDC, &transaction)])).await));

    let reference = fingerprint(&rows[0].1);
    for (what, response) in &rows {
        assert_oidc_failure(response, what);
        assert_eq!(fingerprint(response), reference, "{what} differs byte for byte");
        assert!(set_cookie_value(response, SESSION).is_none(), "{what}: no session cookie");
    }
    assert_eq!(env.gate.started().len(), 1, "only the one success reached the gate");
    println!("OIDC: {} failure rows, all the same 302 /?error=oidc: {}", rows.len(), rows.iter().map(|r| r.0).collect::<Vec<_>>().join(", "));
}

#[tokio::test]
async fn an_error_text_from_the_provider_is_not_reflected() {
    let env = AuthEnv::all();
    let (_, callback, transaction) = env.oidc_begin().await;
    let request = env.get(&format!(
        "/auth/oidc/callback?error=%3Cscript%3E&error_description=secret-reason-text&state={}&iss=https%3A%2F%2Fevil.example",
        callback.state
    ));
    let response = env.send(request.set_cookies(&[(OIDC, &transaction)])).await;
    assert_oidc_failure(&response, "a hostile error text");
    assert!(!response.text().contains("secret-reason-text") && response.body.is_empty());
}

#[tokio::test]
async fn a_malformed_or_repeated_callback_query_is_the_same_redirect() {
    let env = AuthEnv::all();
    for (what, query) in [
        ("a bad escape", "code=%ZZ&state=x".to_owned()),
        ("a repeated code", "code=a&code=b&state=x".to_owned()),
        ("no query at all", String::new()),
    ] {
        let (_, _, transaction) = env.oidc_begin().await;
        let path = if query.is_empty() { "/auth/oidc/callback".to_owned() } else { format!("/auth/oidc/callback?{query}") };
        let response = env.send(env.get(&path).set_cookies(&[(OIDC, &transaction)])).await;
        assert_oidc_failure(&response, what);
    }
}

#[tokio::test]
async fn an_oidc_id_that_is_not_configured_or_not_well_formed_is_404() {
    let env = AuthEnv::all();
    let control = env.send(env.get("/auth/oidc/corp/start")).await;
    assert_eq!(control.status, 302, "the configured id works: {}", control.text());
    let asked = env.provider.as_ref().expect("provider").requests.load(std::sync::atomic::Ordering::SeqCst);
    let long = "a".repeat(33);
    let ok_shape_unknown = "a".repeat(32);
    for path in [
        "/auth/oidc/nope/start".to_owned(),
        "/auth/oidc/CORP/start".to_owned(),
        "/auth/oidc/a/b/start".to_owned(),
        format!("/auth/oidc/{long}/start"),
        format!("/auth/oidc/{ok_shape_unknown}/start"),
        "/auth/oidc//start".to_owned(),
        "/auth/oidc/a%2Fb/start".to_owned(),
        "/auth/oidc/co%72p%00/start".to_owned(),
    ] {
        let response = env.send(env.get(&path)).await;
        assert_eq!(response.status, 404, "{path}: {}", response.text());
        assert_eq!(response.json(), json!({"code": "not_found"}), "{path}");
        assert!(response.all("set-cookie").is_empty(), "{path}: no cookie");
    }
    assert_eq!(env.provider.as_ref().expect("provider").requests.load(std::sync::atomic::Ordering::SeqCst), asked, "the provider was not asked");
    println!("OIDC: ids nope, CORP, a/b, 33 characters, empty, a%2Fb, a NUL byte: all 404 with no cookie");
}

#[tokio::test]
async fn a_provider_that_cannot_be_reached_gives_the_fixed_redirect_at_the_start() {
    let env = AuthEnv::all();
    env.provider.as_ref().expect("provider").set(|b| b.discovery_status = 500);
    let response = env.send(env.get("/auth/oidc/corp/start")).await;
    assert_oidc_failure(&response, "discovery fails at the start");
    assert_eq!(env.count("signin.fail"), 1);
}

#[tokio::test]
async fn a_provider_that_stops_answering_between_start_and_callback_gives_the_fixed_redirect() {
    let env = AuthEnv::all();
    let (_, callback, transaction) = env.oidc_begin().await;
    env.provider.as_ref().expect("provider").set(|b| b.unreachable = true);
    let response = env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await;
    assert_oidc_failure(&response, "the provider is gone at the callback");
    assert_eq!(env.gate.started().len(), 0);
}

#[tokio::test]
async fn an_oidc_sign_in_ends_the_session_the_browser_held() {
    let env = AuthEnv::all();
    env.person("bob").await;
    let old = env.password_sign_in("bob").await;
    let (_, callback, transaction) = env.oidc_begin().await;
    let response = env
        .send(env.oidc_callback_request(&callback.code, &callback.state, None).set_cookies(&[(SESSION, &old.cookie), (OIDC, &transaction)]))
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert!(env.sessions.lookup(&old.cookie, env.clock.unix()).is_err(), "the held session was ended");
    assert_eq!(env.session_rows(old.account), 0);
}

#[tokio::test]
async fn a_failed_oidc_callback_does_not_end_the_session_the_browser_holds() {
    let env = AuthEnv::all();
    env.person("bob").await;
    let old = env.password_sign_in("bob").await;
    let (_, callback, transaction) = env.oidc_begin().await;
    let response = env
        .send(env.oidc_callback_request(&callback.code, "wrong", None).set_cookies(&[(SESSION, &old.cookie), (OIDC, &transaction)]))
        .await;
    assert_oidc_failure(&response, "wrong state with a held session");
    assert!(env.sessions.lookup(&old.cookie, env.clock.unix()).is_ok(), "a failed attempt leaves the session alone");
}

#[tokio::test]
async fn oidc_routes_take_a_get_only() {
    let env = AuthEnv::all();
    let post_start = env.send(env.post("/auth/oidc/corp/start", &json!({}))).await;
    assert_eq!(post_start.status, 405);
    let post_callback = env.send(env.post("/auth/oidc/callback", &json!({}))).await;
    assert_eq!(post_callback.status, 405);
}

// ---------------------------------------------------------------------------------------------------------------------
// The method matrix
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_method_that_is_off_has_no_route() {
    let mut rows: Vec<String> = Vec::new();
    for methods in auth_env::Methods::combinations() {
        let env = AuthEnv::new(methods);
        let probes: Vec<(&str, &str, Option<Value>, bool)> = vec![
            ("POST", "/auth/password", Some(json!({"username": "x", "password": "y"})), methods.password),
            ("POST", "/auth/totp", Some(json!({"code": "123456"})), methods.password),
            ("POST", "/auth/passkey/begin", Some(json!({})), methods.passkey),
            ("POST", "/auth/passkey/finish", Some(json!({"credential": {}})), methods.passkey),
            ("GET", "/auth/oidc/corp/start", None, methods.oidc),
            ("GET", "/auth/oidc/callback", None, methods.oidc),
        ];
        let mut statuses = Vec::new();
        for (method, path, body, on) in probes {
            let req = match &body {
                Some(body) => env.post(path, body),
                None => env.get(path),
            };
            let response = env.send(req).await;
            statuses.push(format!("{path}={}", response.status.as_u16()));
            if on {
                assert_ne!(response.status, 404, "{} {method} {path} must exist", methods.label());
            } else {
                assert_eq!(response.status, 404, "{} {method} {path} must be absent", methods.label());
                assert_eq!(response.json(), json!({"code": "not_found"}), "{} {path}", methods.label());
                assert!(response.all("set-cookie").is_empty(), "{} {path}: no cookie", methods.label());
            }
        }
        // The session report says the same.
        let me = env.send(env.get("/api/session")).await.json();
        assert_eq!(me["methods"]["passkey"], methods.passkey, "{}", methods.label());
        assert_eq!(me["methods"]["password"], methods.password, "{}", methods.label());
        assert_eq!(me["methods"]["oidc"].as_array().map(Vec::len), Some(usize::from(methods.oidc)), "{}", methods.label());
        rows.push(format!("{}: {}", methods.label(), statuses.join(" ")));
    }
    println!("Matrix:\n{}", rows.join("\n"));
}

#[tokio::test]
async fn the_same_person_signing_in_twice_through_the_provider_is_one_account() {
    let env = AuthEnv::all();
    let first = env.oidc_sign_in().await;
    let second = env.oidc_sign_in().await;
    assert_eq!((first.status.as_u16(), second.status.as_u16()), (200, 200));
    let (one, two) = (env.signed_from(&first), env.signed_from(&second));
    assert_eq!(one.account, two.account, "the same identity is the same account");
    assert_eq!(env.count("signin.ok"), 2);
    assert_ne!(one.cookie, two.cookie);
}

#[tokio::test]
async fn a_pending_cookie_named_twice_is_no_cookie() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let doubled = format!("{PENDING}={pending}; {PENDING}={pending}");
    let response = env.send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set("cookie", &doubled)).await;
    assert_invalid(&response, "the pending cookie twice");
    // Once, it works: the state was not used up by the refused request.
    let once = env.send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &pending)])).await;
    assert_eq!(once.status, 200, "{}", once.text());
}

#[tokio::test]
async fn a_dead_session_cookie_does_not_stop_a_sign_in() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let stale = "A".repeat(43);
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let second = env
        .send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(SESSION, &stale), (PENDING, &pending)]))
        .await;
    assert_eq!(second.status, 200, "{}", second.text());
    assert_ne!(set_cookie_value(&second, SESSION).as_deref(), Some(stale.as_str()), "a value from the request is never adopted");
}

// ---------------------------------------------------------------------------------------------------------------------
// Logout
// ---------------------------------------------------------------------------------------------------------------------

use std::sync::Arc;

use auth_env::{Methods, PASSPHRASE, RECOVERY, Tuning};
use holzwave_server::http::routes_unlock::{unlock_account_key, unlock_address_key};
use holzwave_server::limits::Limits;
use holzwave_server::store::Role;

const SESSION_GONE: &str = "__Host-hw_session=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0";

/// A session of `name` made by the manager, with the password method.
fn signed(env: &AuthEnv, name: &str) -> edge::Signed {
    env.session_for(name, SignInMethod::PasswordTotp)
}

#[tokio::test]
async fn logout_ends_the_session_deletes_the_cookie_and_writes_one_line() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    let response = env.send(env.post_as("/auth/logout", &json!({}), &who)).await;
    assert_eq!(response.status, 204, "{}", response.text());
    assert!(response.body.is_empty());
    assert_eq!(response.all("set-cookie"), vec![SESSION_GONE.to_owned()]);
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(env.session_rows(alice), 0);
    let lines: Vec<Value> = env.audit_lines().into_iter().filter(|l| l["action"] == "signout").collect();
    assert_eq!(lines.len(), 1, "one signout line");
    assert_eq!((lines[0]["actor"].as_str(), lines[0]["result"].as_str()), (Some(alice.get().to_string().as_str()), Some("ok")));
    println!("Logout: 204, set-cookie [{SESSION_GONE}], one signout line, session rows 0");
}

#[tokio::test]
async fn after_a_logout_the_old_cookie_gets_401_and_a_second_logout_too() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    env.send(env.post_as("/auth/logout", &json!({}), &who)).await;
    let again = env.send(env.post_as("/auth/logout", &json!({}), &who)).await;
    assert_eq!(again.status, 401, "{}", again.text());
    assert_eq!(again.json(), json!({"code": "invalid"}));
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(me.json()["state"], "anonymous");
    assert_eq!(env.count("signout"), 1, "the refused second logout wrote no line");
}

#[tokio::test]
async fn logout_needs_the_token_and_an_empty_object() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    let no_token = env.send(env.post("/auth/logout", &json!({})).set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(no_token.status, 403);
    let with_member = env.send(env.post_as("/auth/logout", &json!({"all": true}), &who)).await;
    assert_eq!(with_member.status, 400);
    let get = env.send(env.get("/auth/logout").set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(get.status, 405);
    assert_eq!(env.session_rows(alice), 1, "none of the three ended the session");
    assert_eq!(env.count("signout"), 0);
}

#[tokio::test]
async fn logout_ends_only_the_presented_session() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let first = signed(&env, "alice");
    let second = signed(&env, "alice");
    assert_eq!(env.session_rows(alice), 2);
    env.send(env.post_as("/auth/logout", &json!({}), &first)).await;
    assert_eq!(env.session_rows(alice), 1);
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &second.cookie)])).await;
    assert_eq!(me.json()["state"], "signed_in", "the other browser stays signed in");
}

// ---------------------------------------------------------------------------------------------------------------------
// Unlock
// ---------------------------------------------------------------------------------------------------------------------

async fn unlock(env: &AuthEnv, body: &Value, who: &edge::Signed) -> Resp {
    env.send(env.post_as("/auth/unlock", body, who)).await
}

#[tokio::test]
async fn unlock_with_the_passphrase_calls_the_gate_once() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    env.gate.set_locked(alice, true);
    let response = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json(), json!({"state": "unlocked"}));
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    let calls = env.gate.unlocks();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].account, calls[0].kind, calls[0].secret.as_str()), (alice, "passphrase", PASSPHRASE));
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(me.json()["state"], "signed_in", "the store is open now");
    println!("Unlock: passphrase 200 {} gate calls 1", response.text());
}

#[tokio::test]
async fn unlock_with_a_recovery_code_calls_the_gate_once() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    let response = unlock(&env, &json!({"recovery_code": RECOVERY}), &who).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json(), json!({"state": "unlocked"}));
    let calls = env.gate.unlocks();
    assert_eq!((calls.len(), calls[0].account, calls[0].kind), (1, alice, "recovery_code"));
}

#[tokio::test]
async fn unlock_refuses_a_body_that_is_not_exactly_one_secret() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    let cases: Vec<(&str, Value, &str)> = vec![
        ("both members", json!({"passphrase": PASSPHRASE, "recovery_code": RECOVERY}), "passphrase"),
        ("neither", json!({}), "passphrase"),
        ("a passphrase of 257 bytes", json!({"passphrase": "p".repeat(257)}), "passphrase"),
        ("a recovery code of 65 characters", json!({"recovery_code": "R".repeat(65)}), "recovery_code"),
        ("a passphrase that is a number", json!({"passphrase": 12345}), "passphrase"),
        ("a recovery code that is a list", json!({"recovery_code": ["a"]}), "recovery_code"),
        ("an empty passphrase", json!({"passphrase": ""}), "passphrase"),
    ];
    for (what, body, field) in &cases {
        let response = unlock(&env, body, &who).await;
        assert_eq!(response.status, 400, "{what}: {}", response.text());
        assert_eq!(response.json(), json!({"code": "bad_request", "field": field}), "{what}");
    }
    let unknown = unlock(&env, &json!({"passphrase": PASSPHRASE, "extra": 1}), &who).await;
    assert_eq!(unknown.status, 400);
    let array = unlock(&env, &json!([PASSPHRASE]), &who).await;
    assert_eq!(array.status, 400);
    assert_eq!(env.gate.unlocks().len(), 0, "the gate was not called by any of them");
    // The bounds themselves are fine: 256 bytes and 64 characters reach the gate.
    let at_bound = unlock(&env, &json!({"passphrase": "p".repeat(256)}), &who).await;
    assert_eq!(at_bound.status, 401);
    let code_bound = unlock(&env, &json!({"recovery_code": "R".repeat(64)}), &who).await;
    assert_eq!(code_bound.status, 401);
    assert_eq!(env.gate.unlocks().len(), 2);
    println!("Unlock: {} rows answer 400 bad_request with field, the gate is called 0 times; 256 bytes and 64 characters reach it", cases.len());
}

#[tokio::test]
async fn a_wrong_secret_is_401_and_is_counted() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    let response = unlock(&env, &json!({"passphrase": "not the passphrase"}), &who).await;
    assert_invalid(&response, "a wrong passphrase");
    assert_eq!(env.failures(&unlock_account_key(alice)), 1);
    assert_eq!(env.failures(&unlock_address_key(std::net::IpAddr::from([0u8; 4]))), 1);
}

#[tokio::test]
async fn the_fifth_wrong_secret_locks_and_a_locked_attempt_never_reaches_the_gate() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    let lines_before = env.audit_lines().len();
    for n in 1..=5 {
        let response = unlock(&env, &json!({"passphrase": format!("wrong {n}")}), &who).await;
        assert_invalid(&response, &format!("wrong attempt {n}"));
    }
    assert_eq!(env.gate.unlocks().len(), 5);
    let locked = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    let seconds = locked.json()["retry_seconds"].as_u64().expect("seconds");
    assert!((1..=900).contains(&seconds), "{seconds}");
    assert_eq!(locked.header("retry-after").as_deref(), Some(seconds.to_string().as_str()));
    assert_eq!(env.gate.unlocks().len(), 5, "the right secret during the lock never reached the gate");
    assert_eq!(env.audit_lines().len(), lines_before, "the route wrote no audit line, not for a failure and not for the lock");
    env.clock.advance(450);
    let half = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(half.status, 429);
    assert_eq!(half.json()["retry_seconds"], 450);
    env.clock.advance(450);
    let after = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(after.status, 200, "{}", after.text());
    println!("Unlock: lock after 5 wrong, retry_seconds {seconds} at once and 450 half-way, right secret works at 900 s, gate calls 5 before the lock ended");
}

#[tokio::test]
async fn twenty_wrong_secrets_from_one_address_lock_the_address() {
    let env = AuthEnv::all();
    let mut sessions = Vec::new();
    for n in 0..6 {
        let name = format!("user{n}");
        env.person(&name).await;
        sessions.push(signed(&env, &name));
    }
    for who in sessions.iter().take(5) {
        for _ in 0..4 {
            let response = unlock(&env, &json!({"passphrase": "wrong"}), who).await;
            assert_invalid(&response, "a wrong secret");
        }
    }
    let sixth = unlock(&env, &json!({"passphrase": PASSPHRASE}), &sessions[5]).await;
    assert_eq!(sixth.status, 429, "a person who never failed is locked by the address: {}", sixth.text());
    assert_eq!(env.gate.unlocks().len(), 20);
}

#[tokio::test]
async fn a_sign_in_lock_does_not_lock_the_unlock_and_the_reverse() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    for _ in 0..5 {
        env.send(env.post("/auth/password", &json!({"username": "alice", "password": WRONG}))).await;
    }
    let locked = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(locked.status, 429, "the sign-in is locked");
    let open = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(open.status, 200, "the unlock is not: {}", open.text());

    let other = AuthEnv::all();
    other.person("alice").await;
    let who = signed(&other, "alice");
    for _ in 0..5 {
        unlock(&other, &json!({"passphrase": "wrong"}), &who).await;
    }
    let locked = unlock(&other, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(locked.status, 429, "the unlock is locked");
    let first = other.send(other.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(first.status, 200, "the sign-in is not: {}", first.text());
    println!("Unlock: a locked sign-in leaves the unlock open (200) and a locked unlock leaves the sign-in open (200)");
}

#[tokio::test]
async fn unlock_needs_a_session_and_the_token() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    let anonymous = env.send(env.post("/auth/unlock", &json!({"passphrase": PASSPHRASE}))).await;
    assert_eq!(anonymous.status, 401);
    let no_token = env.send(env.post("/auth/unlock", &json!({"passphrase": PASSPHRASE})).set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(no_token.status, 403);
    assert_eq!(env.gate.unlocks().len(), 0);
}

#[tokio::test]
async fn a_gate_that_cannot_be_reached_is_503_and_not_counted() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    env.gate.unavailable.store(true, std::sync::atomic::Ordering::SeqCst);
    let response = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(response.status, 503, "{}", response.text());
    assert_eq!(response.json(), json!({"code": "busy"}));
    assert_eq!(env.failures(&unlock_account_key(alice)), 0);
}

#[tokio::test]
async fn a_right_secret_clears_the_failures_of_the_account() {
    let env = AuthEnv::all();
    let alice = env.person("alice").await;
    let who = signed(&env, "alice");
    for _ in 0..4 {
        unlock(&env, &json!({"passphrase": "wrong"}), &who).await;
    }
    assert_eq!(env.failures(&unlock_account_key(alice)), 4);
    let ok = unlock(&env, &json!({"passphrase": PASSPHRASE}), &who).await;
    assert_eq!(ok.status, 200);
    assert_eq!(env.failures(&unlock_account_key(alice)), 0);
}

#[test]
fn the_unlock_keys_are_their_own_space() {
    use holzwave_server::auth::throttle::{account_key, address_key};
    use holzwave_server::store::AccountId;
    let ip = std::net::IpAddr::from([192, 0, 2, 7]);
    let one = AccountId::from_raw(1);
    let two = AccountId::from_raw(2);
    let (a, b, c) = (unlock_account_key(one), unlock_account_key(two), unlock_address_key(ip));
    assert!(a != b && a != c && b != c);
    for key in [&a, &b, &c] {
        assert!(!key.starts_with("a:") && !key.starts_with("i:"), "{key} is not in the sign-in space");
        assert!(key.len() <= 80);
    }
    assert_ne!(c, address_key(ip));
    assert_ne!(a, account_key("1"));
    assert_eq!(unlock_address_key(ip), unlock_address_key(ip), "stable");
}

// ---------------------------------------------------------------------------------------------------------------------
// Step-up
// ---------------------------------------------------------------------------------------------------------------------

use auth_env::webauthn_client::SoftPasskey;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

async fn step_up(env: &AuthEnv, body: &Value, who: &edge::Signed) -> Resp {
    env.send(env.post_as("/auth/step-up", body, who)).await
}

async fn page_view(env: &AuthEnv, who: &edge::Signed) -> Value {
    env.send(env.get("/api/session").set_cookies(&[(SESSION, &who.cookie)])).await.json()
}

#[tokio::test]
async fn step_up_by_password_after_the_window_gives_a_fresh_window() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = env.password_sign_in("alice").await;
    env.clock.advance(301);
    assert_eq!(page_view(&env, &who).await["step_up"], 0, "the sign-in is older than the window");
    let response = step_up(&env, &json!({"password": PASSWORD}), &who).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.json(), json!({"step_up": 300}));
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(page_view(&env, &who).await["step_up"], 300);
    assert_eq!(env.count("session.step_up"), 1);
    println!("Step-up: password 200 {} /api/session step_up 300 (was 0)", response.text());
}

#[tokio::test]
async fn a_step_up_never_extends_the_session() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = env.password_sign_in("alice").await;
    env.clock.advance(100);
    let before = page_view(&env, &who).await;
    let row_before = env.sessions.lookup(&who.cookie, env.clock.unix()).expect("alive");
    env.clock.advance(10);
    let response = step_up(&env, &json!({"password": PASSWORD}), &who).await;
    assert_eq!(response.status, 200, "{}", response.text());
    let after = page_view(&env, &who).await;
    let row_after = env.sessions.lookup(&who.cookie, env.clock.unix()).expect("alive");
    assert_eq!(row_after.absolute_expires, row_before.absolute_expires, "the absolute end did not move");
    assert_eq!(
        after["seconds"].as_i64().expect("seconds"),
        before["seconds"].as_i64().expect("seconds") - 10,
        "10 seconds passed and the idle end did not move with the step-up"
    );
}

#[tokio::test]
async fn a_wrong_password_at_step_up_is_401_and_counts_on_the_sign_in_throttle() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = env.password_sign_in("alice").await;
    env.clock.advance(301);
    for n in 1..=5 {
        let response = step_up(&env, &json!({"password": WRONG}), &who).await;
        assert_invalid(&response, &format!("wrong password {n}"));
    }
    let locked = step_up(&env, &json!({"password": PASSWORD}), &who).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    assert_eq!(locked.json()["code"], "throttled");
    assert!(locked.header("retry-after").is_some());
    assert_eq!(page_view(&env, &who).await["step_up"], 0, "the session did not step up");
    println!("Step-up: wrong password 401 x5, then 429 retry_seconds={}", locked.json()["retry_seconds"]);
}

#[tokio::test]
async fn a_password_at_step_up_from_a_session_made_another_way_is_401() {
    let env = AuthEnv::all();
    let (_, mut key) = env.passkey_person("pia");
    let by_passkey = env.passkey_sign_in(&mut key, None).await;
    let who = env.signed_from(&by_passkey);
    env.clock.advance(301);
    let response = step_up(&env, &json!({"password": PASSWORD}), &who).await;
    assert_invalid(&response, "a passkey session with a password");
    let (_, callback, transaction) = env.oidc_begin().await;
    let sso = env.send(env.oidc_callback_request(&callback.code, &callback.state, Some(&transaction))).await;
    let sso = env.signed_from(&sso);
    let response = step_up(&env, &json!({"password": PASSWORD}), &sso).await;
    assert_invalid(&response, "a single sign-on session with a password");
}

#[tokio::test]
async fn a_step_up_body_has_exactly_one_form() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    for (what, body) in [
        ("both forms", json!({"password": PASSWORD, "credential": {"id": "x"}})),
        ("neither form", json!({})),
        ("an unknown member", json!({"password": PASSWORD, "extra": 1})),
        ("a password that is a number", json!({"password": 5})),
        ("an array", json!([PASSWORD])),
    ] {
        let response = step_up(&env, &body, &who).await;
        assert_eq!(response.status, 400, "{what}: {}", response.text());
        assert_eq!(response.json()["code"], "bad_request", "{what}");
    }
    let no_token = env.send(env.post("/auth/step-up", &json!({"password": PASSWORD})).set_cookies(&[(SESSION, &who.cookie)])).await;
    assert_eq!(no_token.status, 403);
}

/// A step-up with the passkey: the begin request, the assertion and the finish request.
async fn passkey_step_up(env: &AuthEnv, who: &edge::Signed, key: &mut SoftPasskey, signer: Option<&mut SoftPasskey>) -> (Resp, Resp) {
    let begin = env.send(env.post_as("/auth/step-up/begin", &json!({}), who)).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let options = begin.json()["options"].clone();
    let credential = match signer {
        None => key.authenticate(&options, &env.origin).expect("answers"),
        Some(other) => {
            let mut forced = options;
            forced["publicKey"]["allowCredentials"] = json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(other.credential_id()) }]);
            other.authenticate(&forced, &env.origin).expect("answers")
        }
    };
    let finish = env
        .send(env.post("/auth/step-up", &json!({"credential": credential})).set_cookies(&[(SESSION, &who.cookie), (PENDING, &pending)]).csrf(&who.csrf))
        .await;
    (begin, finish)
}

#[tokio::test]
async fn step_up_by_passkey_lists_the_accounts_own_credentials_and_steps_up() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let mut key = env.register_passkey("alice");
    let who = signed(&env, "alice");
    env.clock.advance(301);
    let (begin, finish) = passkey_step_up(&env, &who, &mut key, None).await;
    let allowed: Vec<Value> =
        begin.json()["options"]["publicKey"]["allowCredentials"].as_array().expect("a list").iter().map(|c| c["id"].clone()).collect();
    assert_eq!(allowed, [json!(URL_SAFE_NO_PAD.encode(key.credential_id()))], "exactly the account's credential ids");
    let line = set_cookie_line(&begin, PENDING).expect("pending");
    token_of(&line, PENDING, PENDING_TAIL);
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert_eq!(finish.json(), json!({"step_up": 300}));
    assert_eq!(set_cookie_line(&finish, PENDING).as_deref(), Some(PENDING_GONE), "the pending cookie is deleted");
    assert_eq!(page_view(&env, &who).await["step_up"], 300);
    println!("Step-up: passkey begin 200 (allow list = the account's credential id, pending cookie set), finish 200 {}", finish.text());
}

#[tokio::test]
async fn another_accounts_passkey_does_not_step_this_session_up() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let mut alice_key = env.register_passkey("alice");
    env.person("bob").await;
    let mut bob_key = env.register_passkey("bob");
    let who = signed(&env, "alice");
    env.clock.advance(301);
    let (_, finish) = passkey_step_up(&env, &who, &mut alice_key, Some(&mut bob_key)).await;
    assert_invalid(&finish, "bob's assertion for alice's session");
    assert_eq!(page_view(&env, &who).await["step_up"], 0);
    assert_eq!(env.count("session.step_up"), 0);
}

#[tokio::test]
async fn a_passkey_step_up_begin_for_an_account_without_passkeys_is_409() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    let begin = env.send(env.post_as("/auth/step-up/begin", &json!({}), &who)).await;
    assert_eq!(begin.status, 409, "{}", begin.text());
    assert_eq!(begin.json(), json!({"code": "conflict"}));
    assert!(begin.all("set-cookie").is_empty());
    let anonymous = env.send(env.post("/auth/step-up/begin", &json!({}))).await;
    assert_eq!(anonymous.status, 401);
}

#[tokio::test]
async fn a_passkey_step_up_with_a_missing_pending_cookie_is_401() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let mut key = env.register_passkey("alice");
    let who = signed(&env, "alice");
    let begin = env.send(env.post_as("/auth/step-up/begin", &json!({}), &who)).await;
    let credential = key.authenticate(&begin.json()["options"], &env.origin).expect("answers");
    let response = step_up(&env, &json!({"credential": credential}), &who).await;
    assert_invalid(&response, "no pending cookie");
}

#[tokio::test]
async fn the_method_matrix_covers_logout_unlock_and_step_up() {
    let mut rows = Vec::new();
    for methods in Methods::combinations() {
        let env = AuthEnv::new(methods);
        env.bare("alice", Role::Member);
        let who = signed(&env, "alice");
        let body = |value: Value| value;
        let probes: Vec<(&str, Value, bool)> = vec![
            ("/auth/logout", body(json!({})), true),
            ("/auth/unlock", body(json!({"passphrase": "p"})), true),
            ("/auth/step-up/begin", body(json!({})), methods.passkey),
            ("/auth/step-up", body(json!({"password": "p"})), methods.password),
            ("/auth/step-up", body(json!({"credential": {"id": "x"}})), methods.passkey),
        ];
        let mut statuses = Vec::new();
        // The logout comes last: it ends the session.
        let mut ordered: Vec<_> = probes.into_iter().collect();
        ordered.rotate_left(1);
        for (path, request_body, on) in ordered {
            let response = env.send(env.post_as(path, &request_body, &who)).await;
            statuses.push(format!("{path}={}", response.status.as_u16()));
            if on {
                assert_ne!(response.status, 404, "{} {path} {request_body} must exist", methods.label());
            } else {
                assert_eq!(response.status, 404, "{} {path} {request_body} must be absent", methods.label());
                assert_eq!(response.json(), json!({"code": "not_found"}));
            }
        }
        rows.push(format!("{}: {}", methods.label(), statuses.join(" ")));
    }
    println!("Matrix (logout, unlock, step-up):\n{}", rows.join("\n"));
}

// ---------------------------------------------------------------------------------------------------------------------
// The throttle through the routes
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn twenty_failures_from_one_address_lock_the_sign_in_for_every_name() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let before = env.argon.derivations();
    let addr = std::net::IpAddr::from([198, 51, 100, 9]);
    for n in 0..20 {
        let response = env.send(env.post("/auth/password", &json!({"username": format!("nobody{n}"), "password": WRONG})).peer(addr)).await;
        assert_invalid(&response, "a failure");
    }
    let derived = env.argon.derivations() - before;
    assert_eq!(derived, 20);
    let locked = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD})).peer(addr)).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    assert_eq!(env.argon.derivations() - before, 20, "a locked attempt derives nothing");
    let elsewhere = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(elsewhere.status, 200, "another address is not locked");
    let totp = env.send(env.post("/auth/totp", &json!({"code": "000000"})).peer(addr)).await;
    assert_eq!(totp.status, 429, "the second step is locked by the address as well");
}

#[tokio::test]
async fn a_locked_sign_in_writes_one_locked_line_and_nothing_after() {
    let env = AuthEnv::all();
    env.person("alice").await;
    for _ in 0..5 {
        env.send(env.post("/auth/password", &json!({"username": "alice", "password": WRONG}))).await;
    }
    assert_eq!(env.count("signin.locked"), 1);
    let lines = env.audit_lines().len();
    for _ in 0..3 {
        let response = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
        assert_eq!(response.status, 429);
    }
    assert_eq!(env.audit_lines().len(), lines, "attempts during the lock write no line");
}

#[tokio::test]
async fn the_lock_answer_is_the_same_for_a_known_and_an_unknown_name() {
    let env = AuthEnv::all();
    env.person("alice").await;
    for name in ["alice", "nobody"] {
        for _ in 0..5 {
            env.send(env.post("/auth/password", &json!({"username": name, "password": WRONG}))).await;
        }
    }
    // The address lock does not apply yet (10 failures); both names are locked by their own key.
    let known = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let unknown = env.send(env.post("/auth/password", &json!({"username": "nobody", "password": PASSWORD}))).await;
    assert_eq!(known.status, 429);
    assert_eq!(fingerprint(&known), fingerprint(&unknown), "status, headers and body are equal byte for byte");
}

// ---------------------------------------------------------------------------------------------------------------------
// Hostile input, overload, the canary and the races
// ---------------------------------------------------------------------------------------------------------------------

const MARKER: &str = "hostile-marker-31337";

/// The routes of the plan and the members each takes.
const ROUTES: [(&str, &[&str], bool); 8] = [
    ("/auth/password", &["username", "password"], false),
    ("/auth/totp", &["code"], false),
    ("/auth/passkey/begin", &[], false),
    ("/auth/passkey/finish", &["credential", "prf"], false),
    ("/auth/logout", &[], true),
    ("/auth/unlock", &["passphrase", "recovery_code"], true),
    ("/auth/step-up", &["password", "credential"], true),
    ("/auth/step-up/begin", &[], true),
];

/// The hostile bodies for one route: label, bytes and content type.
fn hostile_bodies(route: &str, members: &[&str]) -> Vec<(String, Vec<u8>, &'static str)> {
    let json_type = "application/json";
    let first = members.first().copied().unwrap_or("x");
    let mut bodies: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
    let prefix = format!("{{\"{first}\":\"{MARKER}\",\"pad\":\"");
    let pad = 64 * 1024 + 1 - prefix.len() - 2;
    bodies.push(("64 KiB plus one byte".into(), format!("{prefix}{}\"}}", "a".repeat(pad)).into_bytes(), json_type));
    bodies.push(("invalid UTF-8".into(), [b"{\"".as_slice(), first.as_bytes(), b"\":\"\xff\xfe\"}"].concat(), json_type));
    bodies.push(("a JSON array".into(), format!("[\"{MARKER}\",\"{MARKER}\"]").into_bytes(), json_type));
    bodies.push(("nesting of 200 levels".into(), format!("{}\"{MARKER}\"{}", "[".repeat(200), "]".repeat(200)).into_bytes(), json_type));
    bodies.push(("nested objects of 200 levels".into(), format!("{}\"{MARKER}\"{}", "{\"a\":".repeat(200), "}".repeat(200)).into_bytes(), json_type));
    bodies.push(("duplicate keys".into(), format!("{{\"{first}\":\"a\",\"{first}\":\"{MARKER}\"}}").into_bytes(), json_type));
    let nulls: Vec<String> = members.iter().map(|m| format!("\"{m}\":null")).collect();
    if !members.is_empty() {
        bodies.push(("null values".into(), format!("{{{}}}", nulls.join(",")).into_bytes(), json_type));
    }
    bodies.push(("text/plain".into(), format!("{{\"{first}\":\"{MARKER}\"}}").into_bytes(), "text/plain"));
    bodies.push(("a string".into(), format!("\"{MARKER}\"").into_bytes(), json_type));
    bodies.push(("truncated".into(), format!("{{\"{first}\":\"{MARKER}").into_bytes(), json_type));
    match route {
        "/auth/password" => {
            bodies.push(("a username of 129 bytes".into(), format!("{{\"username\":\"{}\",\"password\":\"{MARKER}\"}}", "u".repeat(129)).into_bytes(), json_type));
            bodies.push(("a password of 257 bytes".into(), format!("{{\"username\":\"alice\",\"password\":\"{}\"}}", "p".repeat(257)).into_bytes(), json_type));
        }
        "/auth/totp" => {
            bodies.push(("a code of 7 digits".into(), b"{\"code\":\"1234567\"}".to_vec(), json_type));
            bodies.push(("6 non-ASCII digits".into(), "{\"code\":\"\u{ff16}\u{ff16}\u{ff16}\u{ff16}\u{ff16}\u{ff16}\"}".as_bytes().to_vec(), json_type));
            bodies.push(("a code that is a marker".into(), format!("{{\"code\":\"{MARKER}\"}}").into_bytes(), json_type));
        }
        "/auth/passkey/finish" => {
            bodies.push(("a prf of 30 KiB".into(), format!("{{\"credential\":{{}},\"prf\":\"{}\"}}", "q".repeat(30 * 1024)).into_bytes(), json_type));
            bodies.push(("a credential string".into(), format!("{{\"credential\":\"{MARKER}\"}}").into_bytes(), json_type));
        }
        "/auth/unlock" => {
            bodies.push(("a passphrase of 257 bytes".into(), format!("{{\"passphrase\":\"{}\"}}", "p".repeat(257)).into_bytes(), json_type));
            bodies.push(("a recovery code of 65 characters".into(), format!("{{\"recovery_code\":\"{}\"}}", "R".repeat(65)).into_bytes(), json_type));
        }
        "/auth/step-up" => {
            bodies.push(("a password of 257 bytes".into(), format!("{{\"password\":\"{}\"}}", "p".repeat(257)).into_bytes(), json_type));
            bodies.push(("a credential string".into(), format!("{{\"credential\":\"{MARKER}\"}}").into_bytes(), json_type));
        }
        _ => {}
    }
    bodies
}

#[tokio::test]
async fn hostile_bodies_end_in_a_plain_refusal_and_derive_nothing() {
    let env = AuthEnv::all();
    env.person("alice").await;
    let who = signed(&env, "alice");
    let before = env.argon.derivations();
    let mut count = 0usize;
    let mut worst = std::collections::BTreeMap::<u16, usize>::new();
    for (route, members, needs_session) in ROUTES {
        for (what, bytes, content_type) in hostile_bodies(route, members) {
            let mut req = env.post_raw(route, bytes, content_type);
            if needs_session {
                req = req.signed(&who);
            }
            let response = env.send(req).await;
            let status = response.status.as_u16();
            assert!(status < 500 || status == 503, "{route} {what}: status {status}");
            assert!((400..=429).contains(&status) || status == 503, "{route} {what}: status {status} is not a refusal");
            let all = format!("{}{:?}", response.text(), response.headers);
            assert!(!all.contains(MARKER), "{route} {what}: the input was echoed");
            assert!(!all.contains(&"p".repeat(20)) && !all.contains(&"u".repeat(20)), "{route} {what}: the input was echoed");
            *worst.entry(status).or_default() += 1;
            count += 1;
        }
    }
    assert_eq!(env.argon.derivations(), before, "no hostile body caused a derivation");
    assert_eq!(env.session_rows(who.account), 1, "and none made or ended a session");
    println!("Hostile: {count} inputs over 8 routes, statuses {worst:?}, derivations 0");
}

#[tokio::test]
async fn a_body_of_64_kib_exactly_is_read_and_one_byte_more_is_not() {
    let env = AuthEnv::all();
    let make = |size: usize| {
        let prefix = "{\"code\":\"123456\",\"pad\":\"";
        let pad = size - prefix.len() - 2;
        format!("{prefix}{}\"}}", "a".repeat(pad)).into_bytes()
    };
    let at = env.send(env.post_raw("/auth/totp", make(64 * 1024), "application/json")).await;
    assert_eq!(at.status, 400, "read, then refused as an unknown member");
    let over = env.send(env.post_raw("/auth/totp", make(64 * 1024 + 1), "application/json")).await;
    assert_eq!(over.status, 400);
    assert_eq!(over.json(), json!({"code": "bad_request"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forty_simultaneous_password_posts_with_one_permit_give_busy_answers() {
    let tuning = Tuning { permits: 1, derive_pause: Some(std::time::Duration::from_millis(50)), ..Tuning::default() };
    let env = Arc::new(AuthEnv::build(Methods::ALL, tuning));
    env.person("alice").await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let env = env.clone();
        set.spawn(async move { env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await });
    }
    let (mut ok, mut invalid, mut busy, mut other) = (0, 0, 0, Vec::new());
    while let Some(done) = set.join_next().await {
        let response = done.expect("the task finished");
        match response.status.as_u16() {
            200 => ok += 1,
            401 => invalid += 1,
            503 => {
                busy += 1;
                assert_eq!(response.json(), json!({"code": "busy"}));
                assert!(response.all("set-cookie").is_empty());
            }
            status => other.push(status),
        }
    }
    println!("Overload: 40 posts, 200={ok} 401={invalid} 503={busy} other={other:?}");
    assert!(busy >= 6, "one running and 32 waiting leave at least 7 for the busy answer, got {busy}");
    assert!(other.is_empty(), "no other status: {other:?}");
    let after = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(after.status, 200, "a post after they finished works: {}", after.text());
}

const CANARY: &str = "canary-9f3b7c1e-must-not-appear-anywhere";

#[tokio::test]
async fn a_canary_in_every_input_appears_in_no_answer_header_or_audit_line() {
    let env = AuthEnv::all();
    let id = env.person("alice").await;
    env.set_password_text(id, CANARY).await; // the canary is the real password
    let who = signed(&env, "alice");
    let mut seen: Vec<String> = Vec::new();
    let mut note = |response: &Resp| seen.push(format!("{}\n{:?}", response.text(), response.headers));

    // As a wrong name, a wrong password, a wrong code, a wrong pending cookie.
    note(&env.send(env.post("/auth/password", &json!({"username": CANARY, "password": CANARY}))).await);
    note(&env.send(env.post("/auth/password", &json!({"username": "alice", "password": format!("{CANARY}-wrong")}))).await);
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": CANARY}))).await;
    note(&first);
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    note(&env.send(env.post("/auth/totp", &json!({"code": CANARY})).set_cookies(&[(PENDING, &pending)])).await);
    note(&env.send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &CANARY.replace(' ', "-"))])).await);
    // As the prf value, the credential, the passphrase, the recovery code, the step-up password.
    let (_, _, pending) = env.passkey_begin().await;
    note(&finish_passkey(&env, Some(&pending), json!({"id": CANARY}), Some(json!(CANARY))).await);
    note(&unlock(&env, &json!({"passphrase": CANARY}), &who).await);
    note(&unlock(&env, &json!({"recovery_code": CANARY}), &who).await);
    note(&step_up(&env, &json!({"password": CANARY}), &who).await);
    note(&step_up(&env, &json!({"password": format!("{CANARY}-wrong")}), &who).await);
    // And as the real thing: the sign-in that succeeds with the canary as the password.
    let good = env.send(env.post("/auth/totp", &json!({"code": env.code("alice")})).set_cookies(&[(PENDING, &pending_of(&env).await)])).await;
    note(&good);
    // Cookies and query parameters that carry the canary.
    note(&env.send(env.get(&format!("/auth/oidc/callback?code={CANARY}&state={CANARY}")).set_cookies(&[("__Host-hw_oidc", &CANARY.replace(' ', "-"))])).await);

    for (n, text) in seen.iter().enumerate() {
        assert!(!text.contains(CANARY), "answer {n} holds the canary");
    }
    let audit = env.audit_text();
    assert!(!audit.contains(CANARY), "the audit holds the canary");
    assert!(!audit.contains("canary-"), "nor a part of it");
    let key = env.gate.started().iter().find(|c| c.kind == "password").map(|c| c.key.clone());
    if let Some(key) = key {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        assert!(!audit.contains(&hex) && seen.iter().all(|t| !t.contains(&hex)), "the key is nowhere");
    }
    println!("Canary: {} answers (bodies and every header) and the whole audit directory searched; the canary is nowhere", seen.len());
}

/// A fresh pending cookie for alice with the canary password.
async fn pending_of(env: &AuthEnv) -> String {
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": CANARY}))).await;
    set_cookie_value(&first, PENDING).expect("pending")
}

#[test]
fn the_route_sources_hold_no_print_and_no_log_field() {
    for (name, source) in [
        ("routes_auth.rs", include_str!("../src/http/routes_auth.rs")),
        ("routes_unlock.rs", include_str!("../src/http/routes_unlock.rs")),
    ] {
        for banned in ["println!", "eprintln!", "dbg!", "print!(", "tracing::", "Debug)]\nstruct Password"] {
            assert!(!source.contains(banned), "{name} contains {banned}");
        }
        for (n, line) in source.lines().enumerate() {
            if line.contains("log::event(") {
                assert!(line.contains("&[])"), "{name}:{}: a log line may carry no field: {line}", n + 1);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixteen_simultaneous_correct_codes_for_one_pending_token_make_one_session() {
    let env = Arc::new(AuthEnv::all());
    let alice = env.person("alice").await;
    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, PENDING).expect("pending");
    let code = env.code("alice");
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let (env, pending, code) = (env.clone(), pending.clone(), code.clone());
        set.spawn(async move { env.send(env.post("/auth/totp", &json!({"code": code})).set_cookies(&[(PENDING, &pending)])).await });
    }
    let (mut ok, mut invalid, mut other) = (0, 0, Vec::new());
    while let Some(done) = set.join_next().await {
        match done.expect("finished").status.as_u16() {
            200 => ok += 1,
            401 => invalid += 1,
            status => other.push(status),
        }
    }
    println!("Race: 16 correct /auth/totp posts for one pending token: 200={ok} 401={invalid} other={other:?}");
    assert_eq!((ok, invalid), (1, 15), "{other:?}");
    assert_eq!(env.session_rows(alice), 1);
    assert_eq!(env.gate.started().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixteen_simultaneous_wrong_unlocks_count_sixteen_failures() {
    let tuning = Tuning {
        limits: Limits { password_attempts_per_account: 100, password_attempts_per_address: 100, ..Limits::DEFAULT },
        ..Tuning::default()
    };
    let env = Arc::new(AuthEnv::build(Methods::ALL, tuning));
    let alice = env.person("alice").await;
    let who = Arc::new(signed(&env, "alice"));
    let mut set = tokio::task::JoinSet::new();
    for n in 0..16 {
        let (env, who) = (env.clone(), who.clone());
        set.spawn(async move { env.send(env.post_as("/auth/unlock", &json!({"passphrase": format!("wrong {n}")}), &who)).await });
    }
    let mut invalid = 0;
    while let Some(done) = set.join_next().await {
        if done.expect("finished").status == 401 {
            invalid += 1;
        }
    }
    let account = env.failures(&unlock_account_key(alice));
    let address = env.failures(&unlock_address_key(std::net::IpAddr::from([0u8; 4])));
    println!("Race: 16 wrong unlocks: 401={invalid}, failures on the account key {account}, on the address key {address}");
    assert_eq!((invalid, account, address), (16, 16, 16));
}
