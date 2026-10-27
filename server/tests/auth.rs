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
