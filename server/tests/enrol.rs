//! Account creation through the real router: the bootstrap token and the invitation routes, the password method with an
//! authenticator code and the passkey method, the pending state between `begin` and `finish`, rollback, races, uniform
//! refusals, the token throttle, hostile input and the canary.
//!
//! The router is called as a `tower` service (no socket). The services behind it are the real ones over a scratch store and a
//! manual clock; the secret store is a recording fake of the unlock gate. See `support/enrol_env.rs`.

#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/enrol_env.rs"]
mod enrol_env;

use auth_env::{WithCookies, set_cookie_line, set_cookie_value};
use edge::Resp;
use enrol_env::{
    BOOTSTRAP, EnrolEnv, PASSWORD, PENDING, PENDING_GONE, PENDING_TAIL, SESSION, SESSION_TAIL, is_recovery_code,
};
use holzwave_server::auth::totp;
use holzwave_server::clock::Clock as _;
use holzwave_server::store::{AccountState, Role, UnlockKind};
use serde_json::json;

/// The answer every failed step gives: 401 `invalid`, no cookie, no `Retry-After`.
fn assert_invalid(response: &Resp, what: &str) {
    assert_eq!(response.status, 401, "{what}: {}", response.text());
    assert_eq!(response.json(), json!({"code": "invalid"}), "{what}");
    assert!(response.all("set-cookie").is_empty(), "{what}: a cookie was set");
    assert!(response.header("retry-after").is_none(), "{what}: a retry-after header");
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
}

fn token_of(line: &str, name: &str, tail: &str) -> String {
    let value = line
        .strip_prefix(&format!("{name}="))
        .and_then(|rest| rest.strip_suffix(tail))
        .unwrap_or_else(|| panic!("not the exact cookie line: {line}"));
    assert_eq!(value.len(), 43, "the value is 43 characters");
    value.to_owned()
}

// ---- the tracer: the bootstrap token, the password method, a first administrator and a recovery code ----

#[tokio::test]
async fn tracer_begin_shows_a_secret_and_creates_nothing() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();

    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    assert_eq!(begin.header("cache-control").as_deref(), Some("no-store"));
    let body = begin.json();
    assert_eq!(body["next"], "totp_setup");
    let secret = body["secret"].as_str().expect("a secret");
    assert_eq!(secret.len(), 32, "20 bytes in base32 are 32 characters");
    assert!(secret.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)), "base32: {secret}");
    let uri = body["otpauth"].as_str().expect("an otpauth link");
    assert!(uri.starts_with("otpauth://totp/"), "{uri}");
    assert!(uri.contains(&format!("secret={secret}")), "the link carries the same secret");
    let mut members: Vec<&str> = body.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["next", "otpauth", "secret"]);

    let line = set_cookie_line(&begin, PENDING).expect("a pending cookie");
    token_of(&line, PENDING, PENDING_TAIL);
    assert_eq!(begin.all("set-cookie").len(), 1, "only the pending cookie");
    assert!(set_cookie_value(&begin, SESSION).is_none(), "no session before the code is confirmed");

    // Nothing exists and nothing is spent.
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0, "no account after begin");
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "the token is still redeemable");
    assert!(env.audit_lines().is_empty(), "the audit file is empty after begin");
    assert_eq!(env.enrol.pending_count(), 1);
    println!(
        "Tracer: begin 200 next=totp_setup secret={} characters otpauth={} characters, set-cookie [{PENDING}=<43>{PENDING_TAIL}], accounts 0, audit lines 0",
        secret.len(),
        uri.len()
    );
}

#[tokio::test]
async fn tracer_finish_makes_the_administrator_a_session_and_a_recovery_code() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let secret = env.secret_of(&begin);
    let confirmed_step = totp::step_of(env.clock.unix());

    // A cookie from an earlier session of the browser is ended by the new one.
    let finish = env.finish_code(BOOTSTRAP, &pending, &env.code_for(&secret)).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert_eq!(finish.header("cache-control").as_deref(), Some("no-store"));
    let body = finish.json();
    assert_eq!(body["next"], "recovery");
    let recovery = body["recovery_code"].as_str().expect("a recovery code").to_owned();
    assert!(is_recovery_code(&recovery), "eight groups of four Crockford characters: {recovery}");
    let mut members: Vec<&str> = body.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["next", "recovery_code"]);

    let session_line = set_cookie_line(&finish, SESSION).expect("a session cookie");
    let session = token_of(&session_line, SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&finish, PENDING).as_deref(), Some(PENDING_GONE), "the pending cookie is deleted");
    assert_eq!(finish.all("set-cookie").len(), 2);
    assert!(!finish.text().contains(&session), "the body carries no session value");

    // The store holds the administrator and everything that proves the method.
    let alice = env.store.account_by_username("alice").expect("read").expect("the administrator exists");
    assert_eq!(alice.role, Role::Admin);
    assert_eq!(alice.state, AccountState::Active);
    assert_eq!(alice.uid, *edge::UID_RANGE.start(), "the first user id of the range");
    assert_eq!(alice.display_name, "Alice");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 1);
    assert!(env.store.read(|tx| tx.password_get(alice.id)).expect("read").is_some(), "a password row");
    let sealed = env.store.read(|tx| tx.totp_get(alice.id)).expect("read").expect("a sealed authenticator row");
    assert_eq!(sealed.last_step, confirmed_step, "the confirmed step is the stored last step");
    let methods = env.store.read(|tx| tx.unlock_list(alice.id)).expect("list");
    let kinds: Vec<UnlockKind> = methods.iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [UnlockKind::Password, UnlockKind::Recovery]);
    assert_eq!(env.store.read(|tx| tx.session_list(alice.id)).expect("list").len(), 1, "one session");

    // The cookie works: the person is signed in as an administrator.
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &session)])).await;
    assert_eq!(me.json()["state"], "signed_in");
    assert_eq!(me.json()["account"]["role"], "admin");
    assert_eq!(me.json()["account"]["username"], "alice");

    // One audit line for the whole enrolment, written by the accounts service, none from the session.
    let lines = env.audit_lines();
    assert_eq!(lines.len(), 1, "exactly one audit line: {lines:?}");
    assert_eq!(lines[0]["action"], "bootstrap.redeem");
    assert_eq!(lines[0]["result"], "ok");

    // The gate heard about the new keys once, and about the session once.
    let provisions = env.gate.provisions();
    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].account, alice.id);
    let seen: Vec<(UnlockKind, usize)> = provisions[0].keys.iter().map(|k| (k.kind, k.key.len())).collect();
    assert_eq!(seen, [(UnlockKind::Password, 32), (UnlockKind::Recovery, 32)]);
    assert!(provisions[0].keys.iter().all(|k| k.credential_id.is_none()));
    assert_ne!(provisions[0].keys[0].key, provisions[0].keys[1].key, "two different keys");
    let started = env.gate.started();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].account, alice.id);
    println!(
        "Tracer: begin answer {{next, secret(32), otpauth}}; finish 200 {{next=recovery, recovery_code({} characters)}} set-cookie [{SESSION}=<43>{SESSION_TAIL}] [{PENDING_GONE}]",
        recovery.len()
    );
    println!(
        "Tracer: store holds admin alice uid {} display Alice, password row, sealed authenticator row last_step {} (the confirmed step), unlock rows {:?}, 1 session",
        alice.uid, sealed.last_step, kinds
    );
    println!(
        "Tracer: audit {} line(s): {} {}; gate: provision x{} keys {:?} of 32 bytes, session_started x{} kind {}",
        lines.len(),
        lines[0]["action"],
        lines[0]["result"],
        provisions.len(),
        provisions[0].keys.iter().map(|k| k.kind.as_str()).collect::<Vec<_>>(),
        started.len(),
        started[0].kind
    );
}

#[tokio::test]
async fn tracer_the_confirmed_code_cannot_sign_in_at_once() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let (finish, secret) = env.enrol_with_password(BOOTSTRAP, &token, "alice").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let code = env.code_for(&secret);

    let first = env.send(env.post("/auth/password", &json!({"username": "alice", "password": PASSWORD}))).await;
    assert_eq!(first.status, 200, "the password is right: {}", first.text());
    let pending = set_cookie_value(&first, PENDING).expect("a pending cookie");
    let second = env.send(env.post("/auth/totp", &json!({"code": code})).set_cookies(&[(PENDING, &pending)])).await;
    assert_invalid(&second, "the code that confirmed the enrolment");
    println!("Tracer: the confirmed code at sign-in: {} {}", second.status, second.text());
}

#[tokio::test]
async fn tracer_the_recovery_code_appears_in_one_answer_only() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let (finish, _) = env.enrol_with_password(BOOTSTRAP, &token, "alice").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let recovery = finish.json()["recovery_code"].as_str().expect("a recovery code").to_owned();
    let cookie = set_cookie_value(&finish, SESSION).expect("a session cookie");
    assert!(!finish.all("set-cookie").iter().any(|l| l.contains(&recovery)), "not in a cookie");
    assert!(!finish.headers.iter().any(|(_, v)| String::from_utf8_lossy(v.as_bytes()).contains(&recovery)), "not in a header");

    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &cookie)])).await;
    assert!(!me.text().contains(&recovery), "not in the next session answer");
    assert!(!env.audit_text().contains(&recovery), "not in the audit file");
    let plain: String = recovery.chars().filter(|c| *c != '-').collect();
    assert!(!env.audit_text().contains(&plain) && !me.text().contains(&plain), "nor without its dashes");
    println!("Tracer: the recovery code ({} characters) is in the finish answer body and in no cookie, header, session answer or audit line", recovery.len());
}

#[tokio::test]
async fn tracer_a_wrong_code_creates_nothing_and_keeps_the_token() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!(begin.status, 200, "begin: {}", begin.text());
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let secret = env.secret_of(&begin);
    let wrong = {
        let accepted: Vec<String> =
            [-30, 0, 30].iter().map(|d| totp::code_at(&secret, env.clock.unix() + d).expect("code").to_string()).collect();
        (0..1_000_000).map(|n| format!("{n:06}")).find(|c| !accepted.contains(c)).expect("a code")
    };
    let refused = env.finish_code(BOOTSTRAP, &pending, &wrong).await;
    assert_invalid(&refused, "a wrong code");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "the token is still redeemable");
    assert!(env.gate.provisions().is_empty() && env.gate.started().is_empty());
    // The right code still works on the same pending state.
    let finish = env.finish_code(BOOTSTRAP, &pending, &env.code_for(&secret)).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
}

#[tokio::test]
async fn tracer_the_spent_token_is_gone_and_a_finish_without_begin_is_refused() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let nothing = env.send(env.post(BOOTSTRAP, &json!({"step": "finish", "code": "123456"}))).await;
    assert_invalid(&nothing, "a finish with no pending cookie");

    let (finish, _) = env.enrol_with_password(BOOTSTRAP, &token, "alice").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let again = env.begin_password(BOOTSTRAP, &token, "bob", PASSWORD).await;
    assert_eq!(again.status, 410, "{}", again.text());
    assert_eq!(again.json(), json!({"code": "gone"}));
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 1);
}

#[tokio::test]
async fn tracer_peek_is_not_offered_for_the_bootstrap_token() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let peek = env.send(env.post(BOOTSTRAP, &json!({"step": "peek", "token": token}))).await;
    assert_eq!(peek.status, 400, "{}", peek.text());
    assert_eq!(peek.json()["code"], "bad_request");
    assert!(!peek.text().contains(&token), "the token is not echoed");
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "and the token is not spent");
}

#[test]
fn tracer_the_route_source_holds_no_print_and_no_log_field() {
    let source = include_str!("../src/http/routes_enrol.rs");
    for banned in ["println!", "eprintln!", "dbg!", "print!(", "tracing::", "unsafe"] {
        assert!(!source.contains(banned), "routes_enrol.rs contains {banned}");
    }
    for (n, line) in source.lines().enumerate() {
        if line.contains("log::event(") {
            assert!(line.contains("&[])"), "routes_enrol.rs:{}: a log line may carry no field: {line}", n + 1);
        }
    }
}
