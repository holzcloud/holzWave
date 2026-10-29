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

use auth_env::webauthn_client::{SoftPasskey, Tweaks};
use auth_env::{Methods, Tuning, WithCookies, fingerprint, prf_text, set_cookie_line, set_cookie_value};
use edge::Resp;
use enrol_env::{
    BOOTSTRAP, EnrolEnv, INVITE, PASSWORD, PENDING, PENDING_GONE, PENDING_TAIL, SESSION, SESSION_TAIL, is_recovery_code,
    user_handle_of,
};
use holzwave_server::auth::totp;
use holzwave_server::clock::Clock as _;
use holzwave_server::store::{AccountId, AccountState, Role, UnlockKind};
use holzwave_server::unlock::kek_from_prf;
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;
use webauthn_rs::prelude::Url;

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

// ---------------------------------------------------------------------------------------------------------------------
// The passkey method
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn passkey_begin_gives_creation_options_with_a_fresh_handle() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    assert_eq!(begin.header("cache-control").as_deref(), Some("no-store"));
    let body = begin.json();
    assert_eq!(body["next"], "passkey_register");
    let options = &body["options"];
    assert_eq!(user_handle_of(options).len(), 16, "a fresh 16-byte handle");
    assert_eq!(options["publicKey"]["attestation"], "none");
    assert_eq!(options["publicKey"]["authenticatorSelection"]["userVerification"], "required");
    assert!(options["publicKey"]["extensions"]["prf"].is_object(), "the PRF request is present");
    assert_eq!(options["publicKey"]["user"]["name"], "alice");
    assert_eq!(options["publicKey"]["user"]["displayName"], "Alice");
    let mut members: Vec<&str> = body.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["next", "options"]);
    token_of(&set_cookie_line(&begin, PENDING).expect("a pending cookie"), PENDING, PENDING_TAIL);
    assert_eq!(begin.all("set-cookie").len(), 1);
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0, "no account after begin");
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "the token is still redeemable");
    assert!(env.audit_lines().is_empty());
    let again = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    assert_ne!(user_handle_of(&again.json()["options"]), user_handle_of(options), "every begin chooses its own handle");
}

#[tokio::test]
async fn passkey_finish_creates_the_administrator_whose_handle_is_the_user_id() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let options = begin.json()["options"].clone();
    let handle = user_handle_of(&options);
    let mut key = SoftPasskey::new();
    let credential = key.register(&options, &env.origin).expect("the authenticator answers");
    let prf = prf_text(7);
    let finish = env.finish_credential(BOOTSTRAP, &pending, &credential, Some(&prf)).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let body = finish.json();
    assert_eq!(body["next"], "recovery");
    assert!(is_recovery_code(body["recovery_code"].as_str().expect("a recovery code")));
    token_of(&set_cookie_line(&finish, SESSION).expect("a session cookie"), SESSION, SESSION_TAIL);
    assert_eq!(set_cookie_line(&finish, PENDING).as_deref(), Some(PENDING_GONE));

    let alice = env.store.account_by_username("alice").expect("read").expect("the administrator exists");
    assert_eq!(alice.role, Role::Admin);
    assert_eq!(alice.handle.to_vec(), handle, "the account handle is the user id of the options");
    let rows = env.store.read(|tx| tx.passkey_list(alice.id)).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].credential_id, key.credential_id());
    let salt = rows[0].prf_salt.clone().expect("a prf salt is stored");
    assert_eq!(salt.len(), 32);
    let methods = env.store.read(|tx| tx.unlock_list(alice.id)).expect("list");
    let kinds: Vec<UnlockKind> = methods.iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [UnlockKind::Prf, UnlockKind::Recovery]);
    assert_eq!(methods[0].credential_id.as_deref(), Some(key.credential_id()));
    assert_eq!(methods[0].salt, salt, "the prf row carries the salt of the passkey");
    assert!(env.store.read(|tx| tx.password_get(alice.id)).expect("read").is_none(), "no password");
    assert!(env.store.read(|tx| tx.totp_get(alice.id)).expect("read").is_none(), "no authenticator");
    let sessions = env.store.read(|tx| tx.session_list(alice.id)).expect("list");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].method, holzwave_server::store::SignInMethod::Passkey);

    let lines = env.audit_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["action"], "bootstrap.redeem");
    let provisions = env.gate.provisions();
    assert_eq!(provisions.len(), 1);
    let kinds: Vec<UnlockKind> = provisions[0].keys.iter().map(|k| k.kind).collect();
    assert_eq!(kinds, [UnlockKind::Prf, UnlockKind::Recovery]);
    let expected = kek_from_prf(&[7u8; 32], &salt).expect("a key");
    assert_eq!(provisions[0].keys[0].key, expected.expose().to_vec(), "the prf key is derived from the output and the stored salt");
    assert_eq!(provisions[0].keys[0].credential_id.as_deref(), Some(key.credential_id()));
    assert_eq!(env.gate.started().len(), 1);
    println!(
        "Passkey: user id == accounts.handle ({} bytes); rows: passkeys 1 (prf_salt {} bytes), unlock {:?}, sessions 1 (passkey); gate provision keys {:?}",
        handle.len(),
        salt.len(),
        kinds,
        provisions[0].keys.iter().map(|k| (k.kind.as_str(), k.key.len())).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn passkey_signs_in_later_to_the_same_account() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let (finish, mut key) = env.enrol_with_passkey(BOOTSTRAP, &token, "alice", Some(&prf_text(7))).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let alice = env.store.account_by_username("alice").expect("read").expect("alice");
    let signed = env.passkey_sign_in(&mut key, Some(&prf_text(7))).await;
    assert_eq!(signed.status, 200, "{}", signed.text());
    let session = env.signed_from(&signed);
    assert_eq!(session.account, alice.id, "the passkey signs in to the account it enrolled");
    println!("Passkey: the same authenticator signs in later through /auth/passkey/begin and /auth/passkey/finish: 200, account {}", alice.username);
}

#[tokio::test]
async fn passkey_without_prf_provisions_only_the_recovery_key() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let (finish, _) = env.enrol_with_passkey(BOOTSTRAP, &token, "alice", None).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let alice = env.store.account_by_username("alice").expect("read").expect("alice");
    let kinds: Vec<UnlockKind> = env.store.read(|tx| tx.unlock_list(alice.id)).expect("list").iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [UnlockKind::Recovery]);
    let provisions = env.gate.provisions();
    assert_eq!(provisions.len(), 1);
    assert_eq!(provisions[0].keys.iter().map(|k| k.kind).collect::<Vec<_>>(), [UnlockKind::Recovery]);
}

#[tokio::test]
async fn passkey_a_prf_of_the_wrong_length_is_no_material() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let (finish, _) = env.enrol_with_passkey(BOOTSTRAP, &token, "alice", Some("c2hvcnQ")).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let provisions = env.gate.provisions();
    assert_eq!(provisions[0].keys.iter().map(|k| k.kind).collect::<Vec<_>>(), [UnlockKind::Recovery]);
}

#[tokio::test]
async fn passkey_a_credential_for_another_origin_is_invalid_and_keeps_the_token() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let mut key = SoftPasskey::new();
    let evil = Url::parse("https://evil.example.test").expect("a url");
    let credential = key.register_with(&begin.json()["options"], &evil, &Tweaks::unchecked()).expect("the unchecked path signs");
    let refused = env.finish_credential(BOOTSTRAP, &pending, &credential, Some(&prf_text(7))).await;
    assert_invalid(&refused, "a credential for another origin");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "the token is still valid");
    assert!(env.audit_lines().is_empty() && env.gate.provisions().is_empty());
    let (again, _) = env.enrol_with_passkey(BOOTSTRAP, &token, "alice", None).await;
    assert_eq!(again.status, 200, "the next try works: {}", again.text());
}

#[tokio::test]
async fn passkey_a_damaged_response_is_invalid_and_creates_nothing() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let mut key = SoftPasskey::new();
    let mut credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    credential["response"]["clientDataJSON"] = json!("AAAA");
    let refused = env.finish_credential(BOOTSTRAP, &pending, &credential, None).await;
    assert_invalid(&refused, "a damaged response");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok());
}

#[tokio::test]
async fn passkey_a_response_for_another_pending_entry_is_invalid() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let first = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let second = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let (pending_a, pending_b) = (set_cookie_value(&first, PENDING).expect("a"), set_cookie_value(&second, PENDING).expect("b"));
    let mut key = SoftPasskey::new();
    let credential = key.register(&first.json()["options"], &env.origin).expect("answers");
    let refused = env.finish_credential(BOOTSTRAP, &pending_b, &credential, None).await;
    assert_invalid(&refused, "a response made for the options of another entry");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    let right = env.finish_credential(BOOTSTRAP, &pending_a, &credential, None).await;
    assert_eq!(right.status, 200, "the entry it was made for accepts it: {}", right.text());
}

#[tokio::test]
async fn passkey_a_replayed_response_is_invalid() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let first = env.invitation(admin, Role::Member);
    let second = env.invitation(admin, Role::Member);

    let begin = env.begin_passkey(INVITE, &first, "erin").await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    let done = env.finish_credential(INVITE, &pending, &credential, None).await;
    assert_eq!(done.status, 200, "{}", done.text());

    // The same cookie again: the entry was used up.
    let again = env.finish_credential(INVITE, &pending, &credential, None).await;
    assert_invalid(&again, "the same cookie again");
    // The same response under a new entry of another invitation: the challenge is not the one it answers.
    let begin = env.begin_passkey(INVITE, &second, "frank").await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let replay = env.finish_credential(INVITE, &pending, &credential, None).await;
    assert_invalid(&replay, "the old response under a new challenge");
    assert!(env.store.account_by_username("frank").expect("read").is_none());
    let next = env.begin_passkey(INVITE, &second, "frank").await;
    assert_eq!(next.status, 200, "the second invitation is still valid");
}

// ---------------------------------------------------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------------------------------------------------

async fn peek(env: &EnrolEnv, token: &str) -> Resp {
    env.send(env.post(INVITE, &json!({"step": "peek", "token": token}))).await
}

/// An administrator, an account the administrator made (no way to sign in yet) and the invitation for it.
fn carol(env: &EnrolEnv) -> (AccountId, AccountId, String) {
    let admin = env.administrator("root");
    let (account, issued) = env.accounts.create_account(admin, None, "carol", "Carol", Role::Member).expect("carol");
    (admin, account.id, issued.token.expose().to_owned())
}

#[tokio::test]
async fn invitation_peek_of_a_new_person_gives_the_role() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    for _ in 0..3 {
        let seen = peek(&env, &token).await;
        assert_eq!(seen.status, 200, "{}", seen.text());
        assert_eq!(seen.json(), json!({"role": "member", "existing": false}));
        assert_eq!(seen.header("cache-control").as_deref(), Some("no-store"));
        assert!(seen.all("set-cookie").is_empty());
    }
    let admin_token = env.invitation(admin, Role::Admin);
    assert_eq!(peek(&env, &admin_token).await.json(), json!({"role": "admin", "existing": false}));
    assert_eq!(env.enrol.pending_count(), 0, "a peek keeps nothing");
    assert!(env.audit_lines().iter().all(|l| l["action"] == "account.invite"), "a peek writes no line");
    println!("Invitations: peek of a new-person invitation {} (three times, the token stays valid)", peek(&env, &token).await.text());
}

#[tokio::test]
async fn invitation_peek_of_an_account_an_administrator_made_names_the_account() {
    let env = EnrolEnv::new();
    let (_, _, token) = carol(&env);
    let seen = peek(&env, &token).await;
    assert_eq!(seen.status, 200, "{}", seen.text());
    assert_eq!(seen.json(), json!({"role": "member", "existing": true, "username": "carol"}));
    println!("Invitations: peek for an account an administrator made {}", seen.text());
}

#[tokio::test]
async fn invitation_the_three_causes_of_a_dead_token_give_one_answer() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let spent = env.invitation(admin, Role::Member);
    let late = env.invitation(admin, Role::Member);
    let (done, _) = env.enrol_with_password(INVITE, &spent, "grace").await;
    assert_eq!(done.status, 200, "{}", done.text());

    let unknown = peek(&env, &format!("hwi_{}", "A".repeat(43))).await;
    let used = peek(&env, &spent).await;
    env.clock.advance(7 * 24 * 3_600 + 1);
    let expired = peek(&env, &late).await;
    for (what, response) in [("unknown", &unknown), ("spent", &used), ("expired", &expired)] {
        assert_eq!(response.status, 410, "{what}: {}", response.text());
        assert_eq!(response.json(), json!({"code": "gone"}), "{what}");
        assert!(response.all("set-cookie").is_empty(), "{what}");
        assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
    }
    assert_eq!(fingerprint(&unknown), fingerprint(&used));
    assert_eq!(fingerprint(&unknown), fingerprint(&expired));
    println!("Invitations: unknown, spent and expired give 410 {} with identical headers and body ({} bytes)", unknown.text(), unknown.body.len());
}

#[tokio::test]
async fn invitation_a_new_member_is_made_with_the_chosen_name_and_account_redeem_is_written() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    let (finish, _) = env.enrol_with_password(INVITE, &token, "grace").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert!(is_recovery_code(finish.json()["recovery_code"].as_str().expect("a recovery code")));
    let grace = env.store.account_by_username("grace").expect("read").expect("grace exists");
    assert_eq!(grace.role, Role::Member);
    assert_eq!(grace.display_name, "Alice", "the display name the person chose");
    assert!(env.store.read(|tx| tx.password_get(grace.id)).expect("read").is_some());
    assert!(env.store.read(|tx| tx.totp_get(grace.id)).expect("read").is_some());
    let kinds: Vec<UnlockKind> = env.store.read(|tx| tx.unlock_list(grace.id)).expect("list").iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [UnlockKind::Password, UnlockKind::Recovery]);
    let actions: Vec<String> = env.audit_lines().iter().map(|l| l["action"].as_str().unwrap_or("").to_owned()).collect();
    assert_eq!(actions, ["account.invite", "account.redeem"], "one redeem line and no bootstrap line");
    assert_eq!(env.audit_lines()[1]["result"], "ok");
    let sessions = env.store.read(|tx| tx.session_list(grace.id)).expect("list");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].method, holzwave_server::store::SignInMethod::PasswordTotp);
    println!("Invitations: a new member enrolled with a password: audit actions {actions:?}, session method password_totp");
}

#[tokio::test]
async fn invitation_with_the_admin_role_makes_an_administrator() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Admin);
    let (finish, _) = env.enrol_with_passkey(INVITE, &token, "heidi", None).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let heidi = env.store.account_by_username("heidi").expect("read").expect("heidi exists");
    assert_eq!(heidi.role, Role::Admin);
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &set_cookie_value(&finish, SESSION).expect("cookie"))])).await;
    assert_eq!(me.json()["account"]["role"], "admin");
}

#[tokio::test]
async fn invitation_a_member_enrols_with_a_passkey() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    let begin = env.begin_passkey(INVITE, &token, "ivan").await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    let handle = user_handle_of(&begin.json()["options"]);
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    let finish = env.finish_credential(INVITE, &pending, &credential, Some(&prf_text(9))).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let ivan = env.store.account_by_username("ivan").expect("read").expect("ivan exists");
    assert_eq!(ivan.role, Role::Member);
    assert_eq!(ivan.handle.to_vec(), handle);
    let signed = env.passkey_sign_in(&mut key, None).await;
    assert_eq!(signed.status, 200, "{}", signed.text());
}

#[tokio::test]
async fn invitation_a_spent_token_is_gone_at_every_step() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    let (finish, _) = env.enrol_with_password(INVITE, &token, "grace").await;
    assert_eq!(finish.status, 200);
    assert_eq!(peek(&env, &token).await.status, 410);
    let begin = env.begin_password(INVITE, &token, "grace2", PASSWORD).await;
    assert_eq!((begin.status.as_u16(), begin.json()), (410, json!({"code": "gone"})));
    let begin = env.begin_passkey(INVITE, &token, "grace2").await;
    assert_eq!(begin.status, 410);
    assert!(env.store.account_by_username("grace2").expect("read").is_none());
}

#[tokio::test]
async fn invitation_a_session_the_browser_held_is_ended() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let old = env.session_for("root", holzwave_server::auth::session::Method::PasswordTotp);
    assert_eq!(env.session_rows(admin), 1);
    let invitation = env.invitation(admin, Role::Member);
    let begin = env.begin_password(INVITE, &invitation, "carol", PASSWORD).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let secret = env.secret_of(&begin);
    let finish = env
        .send(
            env.post(INVITE, &json!({"step": "finish", "code": env.code_for(&secret)}))
                .set_cookies(&[(PENDING, &pending), (SESSION, &old.cookie)]),
        )
        .await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert_eq!(env.session_rows(admin), 0, "the session the browser held is gone");
    let carol = env.store.account_by_username("carol").expect("read").expect("carol exists");
    assert_eq!(env.session_rows(carol.id), 1);
}

#[tokio::test]
async fn invitation_and_bootstrap_tokens_do_not_work_on_each_others_routes() {
    let env = EnrolEnv::new();
    let bootstrap = env.bootstrap_token();
    let wrong = peek(&env, &bootstrap).await;
    assert_eq!((wrong.status.as_u16(), wrong.json()), (410, json!({"code": "gone"})));
    let wrong = env.begin_password(INVITE, &bootstrap, "alice", PASSWORD).await;
    assert_eq!(wrong.status, 410);
    let admin = env.administrator("root");
    // The bootstrap token cannot be issued once an account exists, so the invitation is tried against the other route.
    let invitation = env.invitation(admin, Role::Member);
    let wrong = env.begin_password(BOOTSTRAP, &invitation, "alice", PASSWORD).await;
    assert_eq!(wrong.status, 410);
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 1);
}

#[tokio::test]
async fn the_bootstrap_token_does_not_work_once_an_account_exists() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    env.administrator("root");
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!((begin.status.as_u16(), begin.json()), (410, json!({"code": "gone"})));
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    assert_eq!(begin.status, 410);
    assert!(env.store.account_by_username("alice").expect("read").is_none());
}

#[tokio::test]
async fn the_service_peek_spends_nothing_and_writes_no_line() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    let lines = env.audit_lines().len();
    for _ in 0..5 {
        let seen = env.accounts.peek_invite(&token).expect("a valid invitation");
        assert_eq!(seen.role, Role::Member);
        assert!(seen.account.is_none());
    }
    assert_eq!(env.audit_lines().len(), lines, "no audit line");
    let invites = env.store.read(|tx| tx.invite_list()).expect("list");
    assert!(invites.iter().all(|i| i.redeemed_at.is_none()), "nothing is spent");
    let account = env
        .accounts
        .redeem_invite(&token, &holzwave_server::accounts::NewAccount { username: "zed", display_name: "Zed", handle: None }, None, |_, _| {
            Ok::<(), holzwave_server::accounts::AccountError>(())
        })
        .expect("redeemed after the peeks");
    assert_eq!(account.username, "zed");
    assert!(env.accounts.peek_invite(&token).is_err(), "a spent token is not redeemable");
    println!("Peek: a write transaction that always rolls back (BEGIN IMMEDIATE, the redemption query, then an error); 5 peeks left redeemed_at NULL and no audit line");
}

#[tokio::test]
async fn the_service_peek_tells_an_existing_account_and_whether_it_has_a_method() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    let seen = env.accounts.peek_invite(&token).expect("valid");
    let target = seen.account.expect("an existing account");
    assert_eq!((target.id, target.username.as_str(), target.display_name.as_str()), (id, "carol", "Carol"));
    assert!(!target.has_method);
    let account = env.store.account_get(id).expect("read").expect("carol");
    assert_eq!(target.handle, account.handle);
    env.set_password(id).await;
    let seen = env.accounts.peek_invite(&token).expect("still valid as a token");
    assert!(seen.account.expect("account").has_method, "a password row is a method");
    assert!(env.accounts.peek_bootstrap(&token).is_err());
}

#[tokio::test]
async fn the_service_peek_of_a_bootstrap_token_after_an_account_exists_is_not_redeemable() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    assert!(env.accounts.peek_bootstrap(&token).is_ok());
    env.administrator("root");
    assert_eq!(env.accounts.peek_bootstrap(&token), Err(holzwave_server::accounts::AccountError::NotRedeemable));
    assert_eq!(env.accounts.peek_bootstrap("hwb_short"), Err(holzwave_server::accounts::AccountError::NotRedeemable));
    assert_eq!(env.accounts.peek_bootstrap(""), Err(holzwave_server::accounts::AccountError::NotRedeemable));
}

// ---------------------------------------------------------------------------------------------------------------------
// An invitation for an account that exists
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn existing_the_fixed_username_wins_and_role_and_uid_stay() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    let before = env.store.account_get(id).expect("read").expect("carol");
    let (finish, _) = env.enrol_with_password(INVITE, &token, "mallory").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let after = env.store.account_get(id).expect("read").expect("carol");
    assert_eq!(after, before, "same username, role, uid, handle and display name");
    assert!(env.store.account_by_username("mallory").expect("read").is_none(), "no account under the typed name");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 2);
    assert!(env.store.read(|tx| tx.password_get(id)).expect("read").is_some());
    assert_eq!(env.store.read(|tx| tx.session_list(id)).expect("list").len(), 1);
    let me = env.send(env.get("/api/session").set_cookies(&[(SESSION, &set_cookie_value(&finish, SESSION).expect("cookie"))])).await;
    assert_eq!(me.json()["account"]["username"], "carol");
    println!("Existing: the typed name mallory was ignored; carol kept uid {} and role member; accounts 2", before.uid);
}

#[tokio::test]
async fn existing_the_answer_does_not_reveal_whether_the_typed_name_differs() {
    let env = EnrolEnv::new();
    let (admin, _, token) = carol(&env);
    env.bare("taken", Role::Member);
    let _ = admin;
    let same = env.begin_password(INVITE, &token, "carol", PASSWORD).await;
    let other = env.begin_password(INVITE, &token, "mallory", PASSWORD).await;
    let taken = env.begin_password(INVITE, &token, "taken", PASSWORD).await;
    for (what, response) in [("same", &same), ("other", &other), ("taken", &taken)] {
        assert_eq!(response.status, 200, "{what}: {}", response.text());
        let mut members: Vec<String> = response.json().as_object().expect("an object").keys().cloned().collect();
        members.sort();
        assert_eq!(members, ["next", "otpauth", "secret"], "{what}");
    }
    // The link names the account that exists, whatever was typed.
    for response in [&same, &other, &taken] {
        assert!(response.json()["otpauth"].as_str().expect("link").contains("carol"));
    }
}

#[tokio::test]
async fn existing_an_account_with_a_password_gives_gone() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    env.set_password(id).await;
    for response in [peek(&env, &token).await, env.begin_password(INVITE, &token, "carol", PASSWORD).await, env.begin_passkey(INVITE, &token, "carol").await] {
        assert_eq!((response.status.as_u16(), response.json()), (410, json!({"code": "gone"})));
    }
    assert_eq!(env.store.read(|tx| tx.session_list(id)).expect("list").len(), 0);
}

#[tokio::test]
async fn existing_an_account_with_a_passkey_gives_gone() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let (id, _key) = env.passkey_person("kim");
    let token = env.accounts.invite_create(admin, None, Some(id), Role::Member).expect("an invitation").token.expose().to_owned();
    for response in [peek(&env, &token).await, env.begin_password(INVITE, &token, "kim", PASSWORD).await, env.begin_passkey(INVITE, &token, "kim").await] {
        assert_eq!((response.status.as_u16(), response.json()), (410, json!({"code": "gone"})));
    }
    assert_eq!(env.store.read(|tx| tx.passkey_list(id)).expect("list").len(), 1, "the passkey it had is untouched");
}

#[tokio::test]
async fn existing_an_account_with_a_single_sign_on_identity_gives_gone() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    env.store.write(|tx| tx.oidc_link(id, "https://idp.example.test", "subject-1")).expect("link");
    assert_eq!(peek(&env, &token).await.status, 410);
    assert_eq!(env.begin_password(INVITE, &token, "carol", PASSWORD).await.status, 410);
}

#[tokio::test]
async fn existing_a_passkey_uses_the_handle_of_the_account() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    let account = env.store.account_get(id).expect("read").expect("carol");
    let begin = env.begin_passkey(INVITE, &token, "mallory").await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    assert_eq!(user_handle_of(&begin.json()["options"]), account.handle.to_vec(), "the account's own handle, not a new one");
    assert_eq!(begin.json()["options"]["publicKey"]["user"]["name"], "carol");
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    let finish = env.finish_credential(INVITE, &pending, &credential, None).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let signed = env.passkey_sign_in(&mut key, None).await;
    assert_eq!(signed.status, 200, "{}", signed.text());
    assert_eq!(env.signed_from(&signed).account, id);
}

#[tokio::test]
async fn existing_a_method_added_between_begin_and_finish_gives_gone_and_changes_nothing() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    let begin = env.begin_password(INVITE, &token, "carol", PASSWORD).await;
    let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
    let secret = env.secret_of(&begin);
    env.set_password(id).await;
    let finish = env.finish_code(INVITE, &pending, &env.code_for(&secret)).await;
    assert_eq!((finish.status.as_u16(), finish.json()), (410, json!({"code": "gone"})));
    assert!(env.store.read(|tx| tx.totp_get(id)).expect("read").is_none(), "no authenticator was stored");
    assert!(env.store.read(|tx| tx.unlock_list(id)).expect("list").is_empty(), "no unlock rows");
    assert!(env.store.read(|tx| tx.session_list(id)).expect("list").is_empty(), "no session");
    assert!(env.gate.provisions().is_empty());
    assert_eq!(env.enrol.pending_count(), 0);
    println!("Existing: an account that had a method added after begin gives 410 at finish and the transaction left no row");
}

// ---------------------------------------------------------------------------------------------------------------------
// Which methods are on
// ---------------------------------------------------------------------------------------------------------------------

fn only(passkey: bool, password: bool, oidc: bool) -> EnrolEnv {
    EnrolEnv::build(Methods { passkey, password, oidc }, Tuning::default())
}

#[tokio::test]
async fn availability_a_switched_off_password_method_is_refused_with_the_field() {
    let env = only(true, false, false);
    let token = env.bootstrap_token();
    let refused = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request", "field": "method"})));
    assert_eq!(env.enrol.pending_count(), 0);
    assert_eq!(env.argon.derivations(), 0);
    let allowed = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    assert_eq!(allowed.status, 200, "{}", allowed.text());
}

#[tokio::test]
async fn availability_a_switched_off_passkey_method_is_refused_with_the_field() {
    let env = only(false, true, false);
    let token = env.bootstrap_token();
    let refused = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request", "field": "method"})));
    let allowed = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    assert_eq!(allowed.status, 200, "{}", allowed.text());
}

#[tokio::test]
async fn availability_an_unknown_method_is_refused() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    for method in [json!("PASSWORD"), json!("pass key"), json!(""), json!(7), json!(null), json!(["password"])] {
        let mut body = EnrolEnv::begin_password_body(&token, "alice", "Alice", PASSWORD);
        body["method"] = method.clone();
        let refused = env.send(env.post(BOOTSTRAP, &body)).await;
        assert_eq!(refused.status, 400, "{method}: {}", refused.text());
        assert_eq!(refused.json()["code"], "bad_request");
    }
    let mut body = EnrolEnv::begin_password_body(&token, "alice", "Alice", PASSWORD);
    body.as_object_mut().expect("object").remove("method");
    assert_eq!(env.send(env.post(BOOTSTRAP, &body)).await.status, 400);
    assert_eq!(env.argon.derivations(), 0);
}

#[tokio::test]
async fn availability_single_sign_on_alone_has_no_enrolment_routes() {
    let env = only(false, false, true);
    for route in [BOOTSTRAP, INVITE] {
        let response = env.send(env.post(route, &json!({"step": "peek", "token": "hwi_x"}))).await;
        assert_eq!((response.status.as_u16(), response.json()), (404, json!({"code": "not_found"})), "{route}");
    }
}

#[tokio::test]
async fn availability_by_configuration_route_and_method() {
    let mut table = Vec::new();
    for methods in Methods::combinations() {
        let env = only(methods.passkey, methods.password, methods.oidc);
        let dead = format!("hwi_{}", "A".repeat(43));
        let mut row = Vec::new();
        for route in [BOOTSTRAP, INVITE] {
            for method in ["password", "passkey"] {
                let mut body = json!({"step": "begin", "token": dead, "username": "alice", "method": method});
                if method == "password" {
                    body["password"] = json!(PASSWORD);
                }
                let response = env.send(env.post(route, &body)).await;
                let on = if method == "password" { methods.password } else { methods.passkey };
                let routes_exist = methods.password || methods.passkey;
                let expected = if !routes_exist {
                    404
                } else if on {
                    410
                } else {
                    400
                };
                assert_eq!(response.status.as_u16(), expected, "{} {route} {method}: {}", methods.label(), response.text());
                row.push(response.status.as_u16());
            }
        }
        table.push(format!("{} -> bootstrap password/passkey, invite password/passkey = {row:?}", methods.label()));
    }
    println!("Availability: (a dead token, so that no derivation runs) 404 route absent, 410 method on, 400 method off");
    for line in table {
        println!("Availability: {line}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_username_outside_the_rules_is_refused_with_the_field_and_has_no_effect() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let long = "x".repeat(65);
    for name in ["Alice", "a b", long.as_str(), ".x", "x.", "", "al\u{0}ice", "admin'; drop", "ünï", "a/b", "a\nb"] {
        let refused = env.begin_password(BOOTSTRAP, &token, name, PASSWORD).await;
        assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request", "field": "username"})), "{name:?}");
        let refused = env.begin_passkey(BOOTSTRAP, &token, name).await;
        assert_eq!(refused.status, 400, "{name:?}");
        assert!(!refused.text().contains("drop"), "the name is not echoed");
    }
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert_eq!(env.enrol.pending_count(), 0);
    assert_eq!(env.argon.derivations(), 0, "no derivation for a name that is invalid");
    assert!(env.accounts.peek_bootstrap(&token).is_ok());
}

#[tokio::test]
async fn a_taken_username_is_a_conflict_and_the_token_still_works() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    env.bare("bob", Role::Member);
    let token = env.invitation(admin, Role::Member);
    let refused = env.begin_password(INVITE, &token, "bob", PASSWORD).await;
    assert_eq!((refused.status.as_u16(), refused.json()), (409, json!({"code": "conflict", "field": "username"})));
    assert_eq!(env.enrol.pending_count(), 0);
    assert_eq!(env.argon.derivations(), 0, "no derivation for a name that is taken");
    let refused = env.begin_passkey(INVITE, &token, "root").await;
    assert_eq!(refused.status, 409);
    let (finish, _) = env.enrol_with_password(INVITE, &token, "bob2").await;
    assert_eq!(finish.status, 200, "another name works with the same token: {}", finish.text());
}

#[tokio::test]
async fn a_name_taken_between_begin_and_finish_is_a_conflict_that_keeps_the_token() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let (first, second) = (env.invitation(admin, Role::Member), env.invitation(admin, Role::Member));
    let begin_a = env.begin_password(INVITE, &first, "dave", PASSWORD).await;
    let begin_b = env.begin_password(INVITE, &second, "dave", PASSWORD).await;
    assert_eq!((begin_a.status.as_u16(), begin_b.status.as_u16()), (200, 200), "both may begin: nothing is created yet");
    let (pending_a, pending_b) = (set_cookie_value(&begin_a, PENDING).expect("a"), set_cookie_value(&begin_b, PENDING).expect("b"));
    let (secret_a, secret_b) = (env.secret_of(&begin_a), env.secret_of(&begin_b));
    let done = env.finish_code(INVITE, &pending_a, &env.code_for(&secret_a)).await;
    assert_eq!(done.status, 200, "{}", done.text());
    let lost = env.finish_code(INVITE, &pending_b, &env.code_for(&secret_b)).await;
    assert_eq!((lost.status.as_u16(), lost.json()), (409, json!({"code": "conflict", "field": "username"})));
    assert_eq!(env.enrol.pending_count(), 0, "the entry of the loser is gone");
    assert!(env.accounts.peek_invite(&second).is_ok(), "the token of the loser is valid");
    let (again, _) = env.enrol_with_password(INVITE, &second, "dave2").await;
    assert_eq!(again.status, 200, "{}", again.text());
}

#[tokio::test]
async fn display_names_are_trimmed_and_empty_means_the_user_name() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    for (typed, shown) in [(Some("  Alice Liddell  "), "Alice Liddell"), (Some(""), "n1"), (Some("   "), "n2"), (None, "n3")] {
        let token = env.invitation(admin, Role::Member);
        let name = format!("n{}", match typed { Some("  Alice Liddell  ") => 0, Some("") => 1, Some("   ") => 2, _ => 3 });
        let mut body = EnrolEnv::begin_passkey_body(&token, &name, "x");
        match typed {
            Some(text) => body["display_name"] = json!(text),
            None => {
                body.as_object_mut().expect("object").remove("display_name");
            }
        }
        let begin = env.send(env.post(INVITE, &body)).await;
        assert_eq!(begin.status, 200, "{}", begin.text());
        let pending = set_cookie_value(&begin, PENDING).expect("a pending cookie");
        let mut key = SoftPasskey::new();
        let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
        let finish = env.finish_credential(INVITE, &pending, &credential, None).await;
        assert_eq!(finish.status, 200, "{}", finish.text());
        let account = env.store.account_by_username(&name).expect("read").expect("exists");
        let expected = if typed == Some("  Alice Liddell  ") { shown.to_owned() } else { name.clone() };
        assert_eq!(account.display_name, expected, "typed {typed:?}");
    }
}

#[tokio::test]
async fn display_names_outside_the_rules_are_refused_with_the_field() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let long = "n".repeat(65);
    for name in [long.as_str(), "bad\u{7}name", "zero\u{200b}width", "line\nbreak", "bidi\u{202e}override"] {
        let mut body = EnrolEnv::begin_password_body(&token, "alice", name, PASSWORD);
        body["display_name"] = json!(name);
        let refused = env.send(env.post(BOOTSTRAP, &body)).await;
        assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request", "field": "display_name"})), "{name:?}");
    }
    let sixty_four = "n".repeat(64);
    let mut body = EnrolEnv::begin_password_body(&token, "alice", &sixty_four, PASSWORD);
    body["display_name"] = json!(sixty_four);
    assert_eq!(env.send(env.post(BOOTSTRAP, &body)).await.status, 200, "64 characters are allowed");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
}


// ---------------------------------------------------------------------------------------------------------------------
// Helpers for the harder cases
// ---------------------------------------------------------------------------------------------------------------------

/// The rows of every table a redemption writes, counted on a second connection to the database file, with the spent
/// invitations and bootstrap tokens as two further counts.
fn rows(env: &EnrolEnv) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(env.store.path()).expect("a second connection");
    let mut counts = Vec::new();
    for (label, sql) in [
        ("accounts", "SELECT COUNT(*) FROM accounts"),
        ("password_credentials", "SELECT COUNT(*) FROM password_credentials"),
        ("totp", "SELECT COUNT(*) FROM totp"),
        ("passkeys", "SELECT COUNT(*) FROM passkeys"),
        ("sessions", "SELECT COUNT(*) FROM sessions"),
        ("unlock_methods", "SELECT COUNT(*) FROM unlock_methods"),
        ("spent invitations", "SELECT COUNT(*) FROM invites WHERE redeemed_at IS NOT NULL"),
        ("spent bootstrap tokens", "SELECT COUNT(*) FROM bootstrap WHERE redeemed_at IS NOT NULL"),
    ] {
        let count: i64 = conn.query_row(sql, [], |row| row.get(0)).expect("a count");
        counts.push((label.to_owned(), count));
    }
    counts
}

/// Makes every matching write to `table` fail, from a second connection, the way a full disk or a broken file would.
fn break_writes(env: &EnrolEnv, event: &str, table: &str, when: &str) {
    let conn = rusqlite::Connection::open(env.store.path()).expect("a second connection");
    conn.execute_batch(&format!(
        "CREATE TRIGGER hw_broken BEFORE {event} ON {table} {when} BEGIN SELECT RAISE(ABORT, 'broken'); END;"
    ))
    .expect("the trigger is installed");
}

fn repair(env: &EnrolEnv) {
    let conn = rusqlite::Connection::open(env.store.path()).expect("a second connection");
    conn.execute_batch("DROP TRIGGER hw_broken;").expect("the trigger is dropped");
}

/// A code that none of the three steps the window accepts.
fn wrong_code(env: &EnrolEnv, secret: &[u8]) -> String {
    let now = env.clock.unix();
    let accepted: Vec<String> =
        [-30i64, 0, 30].iter().map(|d| totp::code_at(secret, now + d).expect("a code").to_string()).collect();
    (0..1_000_000).map(|n| format!("{n:06}")).find(|c| !accepted.contains(c)).expect("a code")
}

/// A distinct client address for the number `n`.
fn peer(n: u32) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, ((n >> 8) & 255) as u8, (n & 255) as u8, 1))
}

/// The pending cookie value of a begin that must have succeeded.
fn pending_of(begin: &Resp) -> String {
    assert_eq!(begin.status, 200, "{}", begin.text());
    set_cookie_value(begin, PENDING).expect("a pending cookie")
}

/// The place a failure is injected, for the table-driven rollback tests.
struct Break {
    label: &'static str,
    event: &'static str,
    table: &'static str,
    when: &'static str,
}

fn assert_nothing_left(env: &EnrolEnv, before: &[(String, i64)], what: &str) {
    assert_eq!(rows(env), before, "{what}: a row was left behind");
    assert_eq!(env.enrol.pending_count(), 0, "{what}: the entry was not removed");
    assert!(env.gate.provisions().is_empty(), "{what}: keys were delivered");
    assert!(env.gate.started().is_empty(), "{what}: a session was announced");
    assert!(
        env.audit_lines().iter().all(|line| {
            line["result"] != "ok" || line["action"] == "account.invite" || line["action"] == "account.create"
        }),
        "{what}: a success was written down"
    );
}

// ---------------------------------------------------------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn rollback_every_step_of_the_password_enrolment_leaves_nothing_and_the_token_works() {
    let steps = [
        Break { label: "the token is spent", event: "UPDATE", table: "bootstrap", when: "" },
        Break { label: "the account", event: "INSERT", table: "accounts", when: "" },
        Break { label: "the password", event: "INSERT", table: "password_credentials", when: "" },
        Break { label: "the authenticator", event: "INSERT", table: "totp", when: "" },
        Break { label: "the password unlock row", event: "INSERT", table: "unlock_methods", when: "WHEN NEW.kind = 'password'" },
        Break { label: "the recovery unlock row", event: "INSERT", table: "unlock_methods", when: "WHEN NEW.kind = 'recovery'" },
        Break { label: "the session", event: "INSERT", table: "sessions", when: "" },
    ];
    for step in &steps {
        let env = EnrolEnv::new();
        let token = env.bootstrap_token();
        let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
        let pending = pending_of(&begin);
        let secret = env.secret_of(&begin);
        let before = rows(&env);
        break_writes(&env, step.event, step.table, step.when);
        let finish = env.finish_code(BOOTSTRAP, &pending, &env.code_for(&secret)).await;
        assert_eq!((finish.status.as_u16(), finish.json()), (503, json!({"code": "busy"})), "{}", step.label);
        assert!(finish.all("set-cookie").is_empty(), "{}: a cookie was set", step.label);
        assert!(!finish.text().contains("recovery"), "{}: no recovery code in a failure", step.label);
        assert_nothing_left(&env, &before, step.label);
        repair(&env);
        assert!(env.accounts.peek_bootstrap(&token).is_ok(), "{}: the token still works", step.label);
        let (again, _) = env.enrol_with_password(BOOTSTRAP, &token, "alice").await;
        assert_eq!(again.status, 200, "{}: the next try works: {}", step.label, again.text());
    }
    println!(
        "Rollback: a failing write at each of {} steps of the password enrolment (token, account, password, authenticator, two unlock rows, session) gave 503 busy with no row, no key delivered, no success line, no pending entry and a token that still worked",
        steps.len()
    );
}

#[tokio::test]
async fn rollback_the_invitation_steps_leave_nothing_and_the_invitation_works() {
    let steps = [
        Break { label: "the invitation is spent", event: "UPDATE", table: "invites", when: "" },
        Break { label: "the account", event: "INSERT", table: "accounts", when: "" },
        Break { label: "the session", event: "INSERT", table: "sessions", when: "" },
    ];
    for step in &steps {
        let env = EnrolEnv::new();
        let admin = env.administrator("root");
        let token = env.invitation(admin, Role::Member);
        let begin = env.begin_password(INVITE, &token, "frida", PASSWORD).await;
        let pending = pending_of(&begin);
        let secret = env.secret_of(&begin);
        let before = rows(&env);
        break_writes(&env, step.event, step.table, step.when);
        let finish = env.finish_code(INVITE, &pending, &env.code_for(&secret)).await;
        assert_eq!((finish.status.as_u16(), finish.json()), (503, json!({"code": "busy"})), "{}", step.label);
        assert_nothing_left(&env, &before, step.label);
        repair(&env);
        assert!(env.accounts.peek_invite(&token).is_ok(), "{}: the invitation still works", step.label);
        let (again, _) = env.enrol_with_password(INVITE, &token, "frida").await;
        assert_eq!(again.status, 200, "{}: {}", step.label, again.text());
    }
}

#[tokio::test]
async fn rollback_an_account_that_exists_is_left_as_it_was() {
    let env = EnrolEnv::new();
    let (_, id, token) = carol(&env);
    let account = env.store.account_get(id).expect("read").expect("carol");
    let begin = env.begin_password(INVITE, &token, "carol", PASSWORD).await;
    let (pending, secret) = (pending_of(&begin), env.secret_of(&begin));
    let before = rows(&env);
    break_writes(&env, "INSERT", "unlock_methods", "WHEN NEW.kind = 'recovery'");
    let finish = env.finish_code(INVITE, &pending, &env.code_for(&secret)).await;
    assert_eq!(finish.status, 503, "{}", finish.text());
    assert_nothing_left(&env, &before, "an existing account");
    assert_eq!(env.store.account_get(id).expect("read").expect("carol"), account, "the account is unchanged");
    assert!(env.store.read(|tx| tx.password_get(id)).expect("read").is_none(), "no password was kept");
    repair(&env);
    let (again, _) = env.enrol_with_password(INVITE, &token, "carol").await;
    assert_eq!(again.status, 200, "{}", again.text());
}

#[tokio::test]
async fn rollback_every_step_of_the_passkey_enrolment_leaves_nothing_and_the_token_works() {
    let steps = [
        Break { label: "the passkey", event: "INSERT", table: "passkeys", when: "" },
        Break { label: "the prf unlock row", event: "INSERT", table: "unlock_methods", when: "WHEN NEW.kind = 'prf'" },
        Break { label: "the recovery unlock row", event: "INSERT", table: "unlock_methods", when: "WHEN NEW.kind = 'recovery'" },
        Break { label: "the session", event: "INSERT", table: "sessions", when: "" },
    ];
    for step in &steps {
        let env = EnrolEnv::new();
        let token = env.bootstrap_token();
        let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
        let pending = pending_of(&begin);
        let mut key = SoftPasskey::new();
        let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
        let before = rows(&env);
        break_writes(&env, step.event, step.table, step.when);
        let finish = env.finish_credential(BOOTSTRAP, &pending, &credential, Some(&prf_text(5))).await;
        assert_eq!((finish.status.as_u16(), finish.json()), (503, json!({"code": "busy"})), "{}", step.label);
        assert_nothing_left(&env, &before, step.label);
        repair(&env);
        assert!(env.accounts.peek_bootstrap(&token).is_ok(), "{}: the token still works", step.label);
        let (again, _) = env.enrol_with_passkey(BOOTSTRAP, &token, "alice", Some(&prf_text(5))).await;
        assert_eq!(again.status, 200, "{}: {}", step.label, again.text());
    }
}

#[tokio::test]
async fn rollback_a_gate_that_fails_after_the_commit_changes_nothing_for_the_person() {
    let env = EnrolEnv::new();
    env.gate.fail_provision.store(true, std::sync::atomic::Ordering::SeqCst);
    let token = env.bootstrap_token();
    let (finish, _) = env.enrol_with_password(BOOTSTRAP, &token, "alice").await;
    assert_eq!(finish.status, 200, "the account exists, so the answer says so: {}", finish.text());
    assert!(is_recovery_code(finish.json()["recovery_code"].as_str().expect("a recovery code")));
    assert!(env.store.account_by_username("alice").expect("read").is_some());
    assert_eq!(env.gate.provisions().len(), 1, "the delivery was tried once");
}

// ---------------------------------------------------------------------------------------------------------------------
// Races
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_two_browsers_with_one_bootstrap_token_make_one_administrator() {
    let env = Arc::new(EnrolEnv::new());
    let token = env.bootstrap_token();
    let mut entries = Vec::new();
    for name in ["alice", "bob"] {
        let begin = env.begin_password(BOOTSTRAP, &token, name, PASSWORD).await;
        entries.push((name, pending_of(&begin), env.secret_of(&begin)));
    }
    let mut set = tokio::task::JoinSet::new();
    for (_, pending, secret) in &entries {
        let (env, pending, code) = (env.clone(), pending.clone(), env.code_for(secret));
        set.spawn(async move { env.finish_code(BOOTSTRAP, &pending, &code).await });
    }
    let (mut ok, mut gone, mut other) = (0, 0, Vec::new());
    while let Some(done) = set.join_next().await {
        let response = done.expect("finished");
        match response.status.as_u16() {
            200 => ok += 1,
            410 => gone += 1,
            status => other.push(status),
        }
    }
    println!("Race: 2 browsers, one bootstrap token: 200={ok} 410={gone} other={other:?}");
    assert_eq!((ok, gone), (1, 1), "{other:?}");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 1);
    assert_eq!(env.gate.provisions().len(), 1);
    assert_eq!(env.gate.started().len(), 1);
    let lines = env.audit_lines();
    assert_eq!(lines.iter().filter(|l| l["action"] == "bootstrap.redeem" && l["result"] == "ok").count(), 1);
    assert_eq!(env.enrol.pending_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_sixteen_threads_with_one_invitation_make_one_account() {
    let env = Arc::new(EnrolEnv::new());
    let admin = env.administrator("root");
    let token = env.invitation(admin, Role::Member);
    let mut entries = Vec::new();
    for n in 0..16 {
        let begin = env.begin_password(INVITE, &token, &format!("user{n}"), PASSWORD).await;
        entries.push((pending_of(&begin), env.secret_of(&begin)));
    }
    let mut set = tokio::task::JoinSet::new();
    for (pending, secret) in &entries {
        let (env, pending, code) = (env.clone(), pending.clone(), env.code_for(secret));
        set.spawn(async move { env.finish_code(INVITE, &pending, &code).await });
    }
    let (mut ok, mut gone, mut other) = (0, 0, Vec::new());
    while let Some(done) = set.join_next().await {
        let response = done.expect("finished");
        match response.status.as_u16() {
            200 => ok += 1,
            410 => gone += 1,
            status => other.push(status),
        }
    }
    println!("Race: 16 threads, one invitation: 200={ok} 410={gone} other={other:?}");
    assert_eq!((ok, gone), (1, 15), "{other:?}");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 2, "the administrator and one new account");
    assert_eq!(env.gate.provisions().len(), 1);
    assert_eq!(env.audit_lines().iter().filter(|l| l["action"] == "account.redeem" && l["result"] == "ok").count(), 1);
    assert_eq!(env.enrol.pending_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_two_invitations_for_one_username_make_one_account_and_keep_the_other_token() {
    let env = Arc::new(EnrolEnv::new());
    let admin = env.administrator("root");
    let tokens = [env.invitation(admin, Role::Member), env.invitation(admin, Role::Member)];
    let mut entries = Vec::new();
    for token in &tokens {
        let begin = env.begin_password(INVITE, token, "dave", PASSWORD).await;
        entries.push((pending_of(&begin), env.secret_of(&begin)));
    }
    let mut set = tokio::task::JoinSet::new();
    for (n, (pending, secret)) in entries.iter().enumerate() {
        let (env, pending, code) = (env.clone(), pending.clone(), env.code_for(secret));
        set.spawn(async move { (n, env.finish_code(INVITE, &pending, &code).await) });
    }
    let (mut ok, mut conflict, mut other, mut loser) = (0, 0, Vec::new(), None);
    while let Some(done) = set.join_next().await {
        let (n, response) = done.expect("finished");
        match response.status.as_u16() {
            200 => ok += 1,
            409 => {
                conflict += 1;
                loser = Some(n);
                assert_eq!(response.json(), json!({"code": "conflict", "field": "username"}));
            }
            status => other.push(status),
        }
    }
    println!("Race: two invitations, one username: 200={ok} 409={conflict} other={other:?}");
    assert_eq!((ok, conflict), (1, 1), "{other:?}");
    let loser = loser.expect("one lost");
    assert!(env.accounts.peek_invite(&tokens[loser]).is_ok(), "the token of the loser is valid");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 2);
}

// ---------------------------------------------------------------------------------------------------------------------
// The pending store
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn pending_one_address_keeps_sixteen_entries_and_the_oldest_goes() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let ip = peer(7);
    let mut begins = Vec::new();
    for _ in 0..17 {
        let body = EnrolEnv::begin_passkey_body(&token, "alice", "Alice");
        begins.push(env.send(env.post(BOOTSTRAP, &body).peer(ip)).await);
    }
    assert!(begins.iter().all(|b| b.status == 200));
    assert_eq!(env.enrol.pending_count(), 16, "16 per address");
    // Another address is not touched by that.
    let other = env.send(env.post(BOOTSTRAP, &EnrolEnv::begin_passkey_body(&token, "bob", "Bob")).peer(peer(8))).await;
    assert_eq!(other.status, 200);
    assert_eq!(env.enrol.pending_count(), 17);
    // The first one was the oldest and is gone; the second works.
    let mut key = SoftPasskey::new();
    let first = key.register(&begins[0].json()["options"], &env.origin).expect("answers");
    let refused = env.finish_credential(BOOTSTRAP, &pending_of(&begins[0]), &first, None).await;
    assert_invalid(&refused, "the oldest entry of the address");
    let mut key = SoftPasskey::new();
    let second = key.register(&begins[1].json()["options"], &env.origin).expect("answers");
    let accepted = env.finish_credential(BOOTSTRAP, &pending_of(&begins[1]), &second, None).await;
    assert_eq!(accepted.status, 200, "{}", accepted.text());
    println!("Pending: 17 begins from one address leave 16; the first is refused with the plain 401, the second is accepted");
}

#[tokio::test]
async fn pending_the_store_holds_at_most_1024_and_the_oldest_goes() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let mut first = None;
    let mut last = None;
    for n in 0..1_040u32 {
        let body = EnrolEnv::begin_passkey_body(&token, "alice", "Alice");
        let begin = env.send(env.post(BOOTSTRAP, &body).peer(peer(n))).await;
        assert_eq!(begin.status, 200, "begin {n}: {}", begin.text());
        if n == 0 {
            first = Some(begin);
        } else if n == 1_039 {
            last = Some(begin);
        }
    }
    assert_eq!(env.enrol.pending_count(), 1_024, "the store is bounded");
    let (first, last) = (first.expect("first"), last.expect("last"));
    let mut key = SoftPasskey::new();
    let credential = key.register(&first.json()["options"], &env.origin).expect("answers");
    let refused = env.finish_credential(BOOTSTRAP, &pending_of(&first), &credential, None).await;
    assert_invalid(&refused, "the oldest entry of the store");
    let mut key = SoftPasskey::new();
    let credential = key.register(&last.json()["options"], &env.origin).expect("answers");
    let accepted = env.finish_credential(BOOTSTRAP, &pending_of(&last), &credential, None).await;
    assert_eq!(accepted.status, 200, "the newest is kept: {}", accepted.text());
    println!("Pending: 1,040 begins from 1,040 addresses leave 1,024 entries; the oldest is refused, the newest accepted");
}

#[tokio::test]
async fn pending_an_entry_lives_five_minutes_and_one_second_is_too_late() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let on_time = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&on_time), env.secret_of(&on_time));
    assert!(set_cookie_line(&on_time, PENDING).expect("cookie").contains("Max-Age=300"));
    env.clock.advance(300);
    let late = env.begin_password(BOOTSTRAP, &token, "bob", PASSWORD).await;
    let (late_cookie, late_secret) = (pending_of(&late), env.secret_of(&late));
    env.clock.advance(1);
    let refused = env.finish_code(BOOTSTRAP, &late_cookie, &env.code_for(&late_secret)).await;
    assert_eq!(refused.status, 200, "300 seconds old is still alive: {}", refused.text());
    // The first one is now 301 seconds old.
    let expired = env.finish_code(BOOTSTRAP, &cookie, &env.code_for(&secret)).await;
    assert_ne!(expired.status, 200);
    let env2 = EnrolEnv::new();
    let token2 = env2.bootstrap_token();
    let begin = env2.begin_password(BOOTSTRAP, &token2, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env2.secret_of(&begin));
    env2.clock.advance(301);
    let late = env2.finish_code(BOOTSTRAP, &cookie, &env2.code_for(&secret)).await;
    assert_invalid(&late, "301 seconds old");
    assert_eq!(env2.enrol.pending_count(), 0, "the expired entry is gone");
    assert!(env2.accounts.peek_bootstrap(&token2).is_ok(), "the token is valid");
    println!("Pending: an entry is accepted at 300 s and refused with the plain 401 at 301 s (manual clock)");
}

#[tokio::test]
async fn pending_three_wrong_codes_remove_the_entry() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    for n in 1..=3 {
        let refused = env.finish_code(BOOTSTRAP, &cookie, &wrong_code(&env, &secret)).await;
        assert_invalid(&refused, &format!("wrong code {n}"));
        assert_eq!(env.enrol.pending_count(), usize::from(n < 3), "after wrong code {n}");
    }
    let right = env.finish_code(BOOTSTRAP, &cookie, &env.code_for(&secret)).await;
    assert_invalid(&right, "the right code after the third wrong one");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok());
    println!("Pending: three wrong codes remove the entry; the right code afterwards gets the plain 401");
}

#[tokio::test]
async fn pending_a_cookie_of_the_other_route_is_refused_and_the_entry_survives() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    let code = env.code_for(&secret);
    let refused = env.finish_code(INVITE, &cookie, &code).await;
    assert_invalid(&refused, "the invitation route with a bootstrap cookie");
    assert_eq!(env.enrol.pending_count(), 1, "the entry was not removed by the other route");
    let accepted = env.finish_code(BOOTSTRAP, &cookie, &code).await;
    assert_eq!(accepted.status, 200, "{}", accepted.text());
    // And the sign-in's own pending cookie is the same name but a different store: it finds nothing here.
    let sign_in_cookie = format!("{}", "A".repeat(43));
    assert_invalid(&env.finish_code(BOOTSTRAP, &sign_in_cookie, &code).await, "a cookie that belongs to nothing");
}

#[tokio::test]
async fn pending_a_finish_without_a_begin_is_the_plain_401_in_every_form() {
    let env = EnrolEnv::new();
    let body = json!({"step": "finish", "code": "123456"});
    let unknown = "A".repeat(43);
    let requests: Vec<(&str, edge::Req)> = vec![
        ("no cookie", env.post(BOOTSTRAP, &body)),
        ("an unknown cookie", env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, &unknown)])),
        ("a cookie that is no token", env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, "x")])),
        ("an empty cookie", env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, "")])),
        ("the cookie twice", env.post(BOOTSTRAP, &body).header("cookie", &format!("{PENDING}={unknown}; {PENDING}={unknown}"))),
        ("an oversized cookie", env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, &"B".repeat(5_000))])),
    ];
    let mut prints = Vec::new();
    for (what, request) in requests {
        let response = env.send(request).await;
        assert_invalid(&response, what);
        prints.push(fingerprint(&response));
    }
    assert!(prints.windows(2).all(|pair| pair[0] == pair[1]), "all the answers are byte-identical");
    assert_eq!(env.argon.derivations(), 0);
}

// ---------------------------------------------------------------------------------------------------------------------
// Uniform refusals
// ---------------------------------------------------------------------------------------------------------------------

/// A request from the address number `n`, so that a long list of dead tokens does not meet the throttle.
fn from(env: &EnrolEnv, route: &str, body: &Value, n: u32) -> edge::Req {
    env.post(route, body).peer(peer(1_000 + n))
}

#[tokio::test]
async fn uniformity_every_cause_of_a_dead_token_gives_the_same_410_at_every_step() {
    let env = EnrolEnv::new();
    // The bootstrap token of a store that has accounts: issued while there are none, dead as soon as there is one.
    let bootstrap = env.bootstrap_token();
    let root = env.administrator("root");
    let second = env.administrator("second");

    // The four causes.
    let unknown = format!("hwi_{}", "B".repeat(43));
    let spent = env.invitation(root, Role::Member);
    let (done, _) = env.enrol_with_password(INVITE, &spent, "spent1").await;
    assert_eq!(done.status, 200);
    let orphan = env.accounts.invite_create(second, None, None, Role::Member).expect("invitation").token.expose().to_owned();
    env.set_state("second", AccountState::Disabled);
    let early = env.invitation(root, Role::Member);

    // Pending entries made while their token was still good, whose token then dies.
    let spent_later = env.invitation(root, Role::Member);
    let begin = env.begin_password(INVITE, &spent_later, "later1", PASSWORD).await;
    let (cookie_spent, secret_spent) = (pending_of(&begin), env.secret_of(&begin));
    let (done, _) = env.enrol_with_password(INVITE, &spent_later, "later2").await;
    assert_eq!(done.status, 200);

    let third = env.administrator("third");
    let orphan_later = env.accounts.invite_create(third, None, None, Role::Member).expect("invitation").token.expose().to_owned();
    let begin = env.begin_password(INVITE, &orphan_later, "later3", PASSWORD).await;
    let (cookie_orphan, secret_orphan) = (pending_of(&begin), env.secret_of(&begin));
    env.set_state("third", AccountState::Disabled);

    let mut n = 0;
    let mut answers: Vec<(String, Resp)> = Vec::new();
    for (cause, token) in [("unknown", unknown.as_str()), ("spent", spent.as_str()), ("not allowed", orphan.as_str())] {
        for step in ["peek", "begin password", "begin passkey"] {
            let body = match step {
                "peek" => json!({"step": "peek", "token": token}),
                "begin password" => EnrolEnv::begin_password_body(token, "nobody", "Nobody", PASSWORD),
                _ => EnrolEnv::begin_passkey_body(token, "nobody", "Nobody"),
            };
            n += 1;
            answers.push((format!("{cause} / {step}"), env.send(from(&env, INVITE, &body, n)).await));
        }
    }
    for (what, body) in [
        ("bootstrap token with an account / begin password", EnrolEnv::begin_password_body(&bootstrap, "nobody", "Nobody", PASSWORD)),
        ("bootstrap token with an account / begin passkey", EnrolEnv::begin_passkey_body(&bootstrap, "nobody", "Nobody")),
    ] {
        n += 1;
        answers.push((what.to_owned(), env.send(from(&env, BOOTSTRAP, &body, n)).await));
    }
    // Finish after the token died.
    for (what, cookie, secret) in [("spent", &cookie_spent, &secret_spent), ("not allowed", &cookie_orphan, &secret_orphan)] {
        n += 1;
        let request = from(&env, INVITE, &json!({"step": "finish", "code": env.code_for(secret)}), n).set_cookies(&[(PENDING, cookie)]);
        answers.push((format!("{what} / finish"), env.send(request).await));
    }
    // Expired: an invitation that expires while its entry is alive.
    env.clock.advance(7 * 24 * 3_600 - 100);
    let begin = env.begin_password(INVITE, &early, "later4", PASSWORD).await;
    let (cookie_expired, secret_expired) = (pending_of(&begin), env.secret_of(&begin));
    env.clock.advance(150);
    n += 1;
    let request = from(&env, INVITE, &json!({"step": "finish", "code": env.code_for(&secret_expired)}), n).set_cookies(&[(PENDING, &cookie_expired)]);
    answers.push(("expired / finish".to_owned(), env.send(request).await));
    for step in ["peek", "begin password", "begin passkey"] {
        let body = match step {
            "peek" => json!({"step": "peek", "token": early}),
            "begin password" => EnrolEnv::begin_password_body(&early, "nobody", "Nobody", PASSWORD),
            _ => EnrolEnv::begin_passkey_body(&early, "nobody", "Nobody"),
        };
        n += 1;
        answers.push((format!("expired / {step}"), env.send(from(&env, INVITE, &body, n)).await));
    }

    let reference = &answers[0];
    assert_eq!(reference.1.status, 410);
    assert_eq!(reference.1.json(), json!({"code": "gone"}));
    for (what, response) in &answers {
        assert_eq!(fingerprint(response), fingerprint(&reference.1), "{what} differs from {}", reference.0);
        assert!(response.all("set-cookie").is_empty(), "{what}: a cookie");
    }
    println!(
        "Uniformity: {} answers (unknown, spent, expired and not-allowed tokens at peek, begin with both methods and finish) are byte-identical: status, every header and the body {}",
        answers.len(),
        reference.1.text()
    );
}

#[tokio::test]
async fn uniformity_every_cause_of_a_refused_finish_gives_the_same_401() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let mut answers: Vec<(&str, Resp)> = Vec::new();
    let body = json!({"step": "finish", "code": "123456"});
    answers.push(("no begin", env.send(env.post(BOOTSTRAP, &body)).await));

    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    answers.push(("wrong code", env.finish_code(BOOTSTRAP, &cookie, &wrong_code(&env, &secret)).await));
    answers.push(("code of 7 digits", env.finish_code(BOOTSTRAP, &cookie, "1234567").await));
    answers.push(("the other route", env.finish_code(INVITE, &cookie, &env.code_for(&secret)).await));

    env.clock.advance(301);
    answers.push(("expired", env.finish_code(BOOTSTRAP, &cookie, &env.code_for(&secret)).await));

    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    let done = env.finish_code(BOOTSTRAP, &cookie, &env.code_for(&secret)).await;
    assert_eq!(done.status, 200);
    answers.push(("used up", env.finish_code(BOOTSTRAP, &cookie, &env.code_for(&secret)).await));

    let admin = env.store.account_by_username("alice").expect("read").expect("alice").id;
    let invitation = env.accounts.invite_create(admin, None, None, Role::Member).expect("invitation").token.expose().to_owned();
    let begin = env.begin_passkey(INVITE, &invitation, "erin").await;
    let cookie = pending_of(&begin);
    let mut key = SoftPasskey::new();
    let other_begin = env.begin_passkey(INVITE, &invitation, "erin").await;
    let credential = key.register(&other_begin.json()["options"], &env.origin).expect("answers");
    answers.push(("a response for another challenge", env.finish_credential(INVITE, &cookie, &credential, None).await));

    for (what, response) in &answers {
        assert_invalid(response, what);
    }
    let first = fingerprint(&answers[0].1);
    for (what, response) in &answers {
        assert_eq!(fingerprint(response), first, "{what} differs from the first");
    }
    println!("Uniformity: {} causes of a refused finish give byte-identical 401 answers", answers.len());
}

// ---------------------------------------------------------------------------------------------------------------------
// The token throttle
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn throttle_twenty_dead_tokens_lock_the_address_and_nothing_is_called() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let valid = env.invitation(admin, Role::Member);
    let dead = json!({"step": "peek", "token": format!("hwi_{}", "C".repeat(43))});
    let ip = peer(1);
    for n in 1..=20 {
        let response = env.send(env.post(INVITE, &dead).peer(ip)).await;
        assert_eq!(response.status, 410, "dead token {n}");
    }
    let locked = env.send(env.post(INVITE, &dead).peer(ip)).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    let seconds = locked.json()["retry_seconds"].as_i64().expect("retry_seconds");
    assert!((1..=900).contains(&seconds), "{seconds}");
    assert_eq!(locked.json()["code"], "throttled");
    assert_eq!(locked.header("retry-after"), Some(seconds.to_string()));
    assert!(locked.all("set-cookie").is_empty());

    // A valid token from the locked address gets the same lock, and no service is called.
    let before = rows(&env);
    for body in [
        json!({"step": "peek", "token": valid}),
        EnrolEnv::begin_password_body(&valid, "fay", "Fay", PASSWORD),
        EnrolEnv::begin_passkey_body(&valid, "fay", "Fay"),
    ] {
        let response = env.send(env.post(INVITE, &body).peer(ip)).await;
        assert_eq!(response.status, 429, "{}", response.text());
    }
    assert_eq!(env.argon.derivations(), 0, "no derivation");
    assert_eq!(env.enrol.pending_count(), 0, "no entry");
    assert_eq!(env.passkeys.open_challenges(), 0, "no challenge");
    assert_eq!(rows(&env), before);
    assert!(env.accounts.peek_invite(&valid).is_ok());

    // Another address is not locked, and the lock ends.
    let other = env.send(env.post(INVITE, &json!({"step": "peek", "token": valid})).peer(peer(2))).await;
    assert_eq!(other.status, 200, "{}", other.text());
    env.clock.advance(901);
    let after = env.send(env.post(INVITE, &json!({"step": "peek", "token": valid})).peer(ip)).await;
    assert_eq!(after.status, 200, "{}", after.text());
    println!("Throttle: 20 dead tokens from one address, the 21st answers 429 retry_seconds={seconds} with Retry-After, no derivation, no entry, no challenge, no row; another address unaffected, the lock ends after 900 s");
}

#[tokio::test]
async fn throttle_has_a_key_space_of_its_own() {
    let env = EnrolEnv::new();
    let ip = peer(3);
    let dead = json!({"step": "peek", "token": format!("hwi_{}", "D".repeat(43))});
    for _ in 0..20 {
        env.send(env.post(INVITE, &dead).peer(ip)).await;
    }
    assert_eq!(env.send(env.post(INVITE, &dead).peer(ip)).await.status, 429);
    // The sign-in of the same address is not locked by it.
    let sign_in = env.send(env.post("/auth/password", &json!({"username": "nobody", "password": PASSWORD})).peer(ip)).await;
    assert_eq!(sign_in.status, 401, "{}", sign_in.text());
    // And the keys differ in name.
    let key = holzwave_server::http::routes_enrol::enrol_address_key(ip);
    assert!(!key.starts_with("i:") && !key.starts_with("a:") && !key.starts_with("ua:") && !key.starts_with("ui:"), "{key}");
    assert!(env.failures(&key) >= 20);
    assert_eq!(env.failures(&holzwave_server::auth::throttle::address_key(ip)), 1, "the sign-in counted its own failure only");
}

#[tokio::test]
async fn throttle_a_locked_sign_in_does_not_lock_the_enrolment() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let ip = peer(4);
    let mut locked = false;
    for _ in 0..200 {
        let response = env.send(env.post("/auth/password", &json!({"username": "nobody", "password": PASSWORD})).peer(ip)).await;
        if response.status == 429 {
            locked = true;
            break;
        }
    }
    assert!(locked, "the sign-in locks the address at some point");
    let begin = env.send(env.post(BOOTSTRAP, &EnrolEnv::begin_password_body(&token, "alice", "Alice", PASSWORD)).peer(ip)).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
}

#[tokio::test]
async fn throttle_a_wrong_code_is_not_a_dead_token() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let ip = peer(5);
    let key = holzwave_server::http::routes_enrol::enrol_address_key(ip);
    let begin = env.send(env.post(BOOTSTRAP, &EnrolEnv::begin_password_body(&token, "alice", "Alice", PASSWORD)).peer(ip)).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    for _ in 0..2 {
        let body = json!({"step": "finish", "code": wrong_code(&env, &secret)});
        let refused = env.send(env.post(BOOTSTRAP, &body).peer(ip).set_cookies(&[(PENDING, &cookie)])).await;
        assert_eq!(refused.status, 401);
    }
    assert_eq!(env.failures(&key), 0, "wrong codes are not counted against the token throttle");
    let dead = json!({"step": "begin", "token": format!("hwb_{}", "E".repeat(43)), "username": "x", "method": "passkey"});
    assert_eq!(env.send(env.post(BOOTSTRAP, &dead).peer(ip)).await.status, 410);
    assert_eq!(env.failures(&key), 1, "a dead token is");
}

// ---------------------------------------------------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------------------------------------------------

const MARKER: &str = "hostile-marker-31337";

/// The hostile bodies: label, bytes and content type.
fn hostile_bodies() -> Vec<(String, Vec<u8>, &'static str)> {
    let json_type = "application/json";
    let ok_begin = |extra: &str| format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":\"alice\",\"method\":\"password\",\"password\":\"{MARKER}\"{extra}}}");
    let mut bodies: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
    let prefix = format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"pad\":\"");
    let pad = 64 * 1024 + 1 - prefix.len() - 2;
    bodies.push(("64 KiB plus one byte".into(), format!("{prefix}{}\"}}", "a".repeat(pad)).into_bytes(), json_type));
    bodies.push(("a password of 257 bytes".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":\"alice\",\"method\":\"password\",\"password\":\"{}\"}}", "p".repeat(257)).into_bytes(), json_type));
    bodies.push(("a token of 200 characters".into(), format!("{{\"step\":\"begin\",\"token\":\"{}\",\"username\":\"alice\",\"method\":\"password\",\"password\":\"{MARKER}\"}}", "t".repeat(200)).into_bytes(), json_type));
    bodies.push(("a code of 7 digits".into(), b"{\"step\":\"finish\",\"code\":\"1234567\"}".to_vec(), json_type));
    bodies.push(("a code that is a marker".into(), format!("{{\"step\":\"finish\",\"code\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("Finish with a capital".into(), format!("{{\"step\":\"Finish\",\"code\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("a missing step".into(), format!("{{\"token\":\"{MARKER}\",\"code\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("an extra member".into(), ok_begin(&format!(",\"extra\":\"{MARKER}\"")).into_bytes(), json_type));
    bodies.push(("a step that is a number".into(), b"{\"step\":7}".to_vec(), json_type));
    bodies.push(("a token that is an object".into(), format!("{{\"step\":\"begin\",\"token\":{{\"a\":\"{MARKER}\"}}}}").into_bytes(), json_type));
    bodies.push(("a username that is an array".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":[\"{MARKER}\"]}}").into_bytes(), json_type));
    bodies.push(("a method that is a boolean".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":\"alice\",\"method\":true}}").into_bytes(), json_type));
    bodies.push(("nulls".into(), b"{\"step\":null,\"token\":null,\"username\":null,\"method\":null,\"password\":null,\"code\":null}".to_vec(), json_type));
    bodies.push(("duplicate keys".into(), format!("{{\"step\":\"begin\",\"step\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("a credential string".into(), format!("{{\"step\":\"finish\",\"credential\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("a credential array".into(), format!("{{\"step\":\"finish\",\"credential\":[\"{MARKER}\"]}}").into_bytes(), json_type));
    bodies.push(("a prf of 100 characters".into(), format!("{{\"step\":\"finish\",\"credential\":{{}},\"prf\":\"{}\"}}", "q".repeat(100)).into_bytes(), json_type));
    bodies.push(("a prf that is a number".into(), b"{\"step\":\"finish\",\"credential\":{},\"prf\":1}".to_vec(), json_type));
    bodies.push(("a lone surrogate escape".into(), format!("{{\"step\":\"begin\",\"token\":\"\\ud800{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("an invalid escape".into(), format!("{{\"step\":\"begin\",\"token\":\"\\x{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("invalid UTF-8".into(), [b"{\"step\":\"begin\",\"token\":\"".as_slice(), b"\xff\xfe\"}"].concat(), json_type));
    bodies.push(("a JSON array".into(), format!("[\"{MARKER}\"]").into_bytes(), json_type));
    bodies.push(("a bare string".into(), format!("\"{MARKER}\"").into_bytes(), json_type));
    bodies.push(("null".into(), b"null".to_vec(), json_type));
    bodies.push(("an empty body".into(), Vec::new(), json_type));
    bodies.push(("nesting of 200 levels".into(), format!("{}\"{MARKER}\"{}", "[".repeat(200), "]".repeat(200)).into_bytes(), json_type));
    bodies.push(("nested objects of 200 levels".into(), format!("{}\"{MARKER}\"{}", "{\"a\":".repeat(200), "}".repeat(200)).into_bytes(), json_type));
    bodies.push(("truncated".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}").into_bytes(), json_type));
    bodies.push(("text/plain".into(), ok_begin("").into_bytes(), "text/plain"));
    bodies.push(("a display name with a control character".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":\"alice\",\"display_name\":\"{MARKER}\\u0007\",\"method\":\"password\",\"password\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies.push(("an unknown method".into(), format!("{{\"step\":\"begin\",\"token\":\"{MARKER}\",\"username\":\"alice\",\"method\":\"{MARKER}\"}}").into_bytes(), json_type));
    bodies
}

#[tokio::test]
async fn hostile_bodies_end_in_a_plain_refusal_and_derive_nothing() {
    let env = EnrolEnv::new();
    let before = env.argon.derivations();
    let mut count = 0usize;
    let mut worst = std::collections::BTreeMap::<u16, usize>::new();
    for route in [BOOTSTRAP, INVITE] {
        for (what, bytes, content_type) in hostile_bodies() {
            // A different address for each, so that the dead-token counts of the valid-looking ones do not lock the run.
            let response = env.send(env.post_raw(route, bytes, content_type).peer(peer(2_000 + u32::try_from(count).expect("small")))).await;
            let status = response.status.as_u16();
            assert!(status < 500 || status == 503, "{route} {what}: status {status}");
            assert!((400..=429).contains(&status), "{route} {what}: status {status} is not a refusal");
            let all = format!("{}{:?}", response.text(), response.headers);
            assert!(!all.contains(MARKER), "{route} {what}: the input was echoed");
            assert!(!all.contains(&"p".repeat(20)) && !all.contains(&"t".repeat(20)) && !all.contains(&"q".repeat(20)), "{route} {what}: the input was echoed");
            *worst.entry(status).or_default() += 1;
            count += 1;
        }
    }
    assert_eq!(env.argon.derivations(), before, "no hostile body caused a derivation");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert_eq!(env.enrol.pending_count(), 0);
    assert!(env.audit_lines().iter().all(|l| l["result"] != "ok"), "no success was written down");
    println!("Hostile: {count} inputs over 2 routes, statuses {worst:?}, derivations 0, accounts 0, entries 0");
}

#[tokio::test]
async fn hostile_a_finish_of_the_wrong_shape_keeps_the_entry_and_costs_no_attempt() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_password(BOOTSTRAP, &token, "alice", PASSWORD).await;
    let (cookie, secret) = (pending_of(&begin), env.secret_of(&begin));
    let code = env.code_for(&secret);
    let shapes = [
        (json!({"step": "finish"}), Some("code")),
        (json!({"step": "finish", "code": code, "credential": {}}), None),
        (json!({"step": "finish", "code": code, "prf": "x"}), None),
        (json!({"step": "finish", "credential": {}}), None),
        (json!({"step": "finish", "code": 123456}), Some("code")),
        (json!({"step": "finish", "code": code, "token": "x"}), None),
    ];
    for (body, field) in shapes {
        let response = env.send(env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, &cookie)])).await;
        assert_eq!(response.status, 400, "{body}: {}", response.text());
        match field {
            Some(field) => assert_eq!(response.json(), json!({"code": "bad_request", "field": field})),
            None => assert_eq!(response.json(), json!({"code": "bad_request"})),
        }
    }
    assert_eq!(env.enrol.pending_count(), 1, "the entry is still there");
    let accepted = env.finish_code(BOOTSTRAP, &cookie, &code).await;
    assert_eq!(accepted.status, 200, "no attempt was used: {}", accepted.text());
}

#[tokio::test]
async fn hostile_a_passkey_finish_with_junk_is_the_plain_401_and_a_bad_prf_is_a_400() {
    let env = EnrolEnv::new();
    let token = env.bootstrap_token();
    let begin = env.begin_passkey(BOOTSTRAP, &token, "alice").await;
    let cookie = pending_of(&begin);
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    // A prf that is too long is refused before the entry is looked at.
    let body = json!({"step": "finish", "credential": credential, "prf": "q".repeat(100)});
    let refused = env.send(env.post(BOOTSTRAP, &body).set_cookies(&[(PENDING, &cookie)])).await;
    assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request", "field": "prf"})));
    assert_eq!(env.enrol.pending_count(), 1);
    // A password-shaped finish does not fit a passkey entry and costs nothing.
    let refused = env.send(env.post(BOOTSTRAP, &json!({"step": "finish", "code": "123456"})).set_cookies(&[(PENDING, &cookie)])).await;
    assert_eq!((refused.status.as_u16(), refused.json()), (400, json!({"code": "bad_request"})));
    assert_eq!(env.enrol.pending_count(), 1);
    // Junk credential objects are refused with the plain 401 and use the entry up.
    let junk = env.finish_credential(BOOTSTRAP, &cookie, &json!({"id": MARKER, "response": {}}), None).await;
    assert_invalid(&junk, "a junk credential");
    assert!(!junk.text().contains(MARKER));
    assert_eq!(env.enrol.pending_count(), 0);
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok());
}

// ---------------------------------------------------------------------------------------------------------------------
// Overload
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overload_forty_simultaneous_password_begins_with_one_permit_give_busy_answers() {
    let tuning = Tuning { permits: 1, derive_pause: Some(Duration::from_millis(50)), ..Tuning::default() };
    let env = Arc::new(EnrolEnv::build(Methods { passkey: true, password: true, oidc: false }, tuning));
    let token = env.bootstrap_token();
    let mut set = tokio::task::JoinSet::new();
    for n in 0..40u32 {
        let (env, token) = (env.clone(), token.clone());
        set.spawn(async move {
            let body = EnrolEnv::begin_password_body(&token, &format!("user{n}"), "User", PASSWORD);
            env.send(env.post(BOOTSTRAP, &body).peer(peer(3_000 + n))).await
        });
    }
    let (mut ok, mut busy, mut other) = (0usize, 0usize, Vec::new());
    while let Some(done) = set.join_next().await {
        let response = done.expect("the task finished");
        match response.status.as_u16() {
            200 => ok += 1,
            503 => {
                busy += 1;
                assert_eq!(response.json(), json!({"code": "busy"}));
                assert!(response.all("set-cookie").is_empty(), "a refused begin sets no cookie");
            }
            status => other.push(status),
        }
    }
    println!("Overload: 40 password begins with 1 permit: 200={ok} 503={busy} other={other:?}; pending entries {}", env.enrol.pending_count());
    assert!(busy >= 6, "one running and 32 waiting leave at least 7 for the busy answer, got {busy}");
    assert!(other.is_empty(), "no other status: {other:?}");
    assert_eq!(env.enrol.pending_count(), ok, "no entry is left for a refused begin");
    assert_eq!(env.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert!(env.accounts.peek_bootstrap(&token).is_ok(), "the token is untouched");
}

// ---------------------------------------------------------------------------------------------------------------------
// The canary
// ---------------------------------------------------------------------------------------------------------------------

const CANARY: &str = "canary-9f3b7c1e-must-not-appear-anywhere";

#[tokio::test]
async fn canary_in_every_input_appears_in_no_answer_header_or_audit_line() {
    let env = EnrolEnv::new();
    let admin = env.administrator("root");
    let password_token = env.invitation(admin, Role::Member);
    let passkey_token = env.invitation(admin, Role::Member);
    let mut seen: Vec<String> = Vec::new();
    let mut note = |response: &Resp| seen.push(format!("{}\n{:?}", response.text(), response.headers));

    // Refused inputs: as a user name (a capital makes it invalid), as a display name with a control character, as a token,
    // as a method, as extra members, as a code, a credential and a PRF.
    let loud = CANARY.to_uppercase();
    note(&env.send(env.post(INVITE, &EnrolEnv::begin_password_body(&password_token, &loud, "Name", CANARY))).await);
    note(&env.send(env.post(INVITE, &EnrolEnv::begin_password_body(&password_token, "erin", &format!("{CANARY}\u{7}"), CANARY))).await);
    note(&env.send(env.post(INVITE, &EnrolEnv::begin_password_body(CANARY, "erin", "Name", CANARY))).await);
    note(&env.send(env.post(INVITE, &json!({"step": "begin", "token": password_token, "username": "erin", "method": CANARY}))).await);
    note(&env.send(env.post(INVITE, &json!({"step": "peek", "token": CANARY, CANARY: CANARY}))).await);
    note(&env.send(env.post(INVITE, &json!({"step": "peek", "token": CANARY}))).await);

    // The real thing, with the canary as the password and the display name.
    let begin = env.send(env.post(INVITE, &EnrolEnv::begin_password_body(&password_token, "erin", CANARY, CANARY))).await;
    note(&begin);
    let pending = pending_of(&begin);
    let secret_text = begin.json()["secret"].as_str().expect("a secret").to_owned();
    let secret = env.secret_of(&begin);
    note(&env.finish_code(INVITE, &pending, CANARY).await);
    let finish = env.finish_code(INVITE, &pending, &env.code_for(&secret)).await;
    note(&finish);
    assert_eq!(finish.status, 200, "{}", finish.text());
    let recovery = finish.json()["recovery_code"].as_str().expect("a recovery code").to_owned();
    let session = set_cookie_value(&finish, SESSION).expect("a session cookie");

    // The passkey method with the canary as the PRF, a credential member and a credential name.
    let begin = env.begin_passkey(INVITE, &passkey_token, "gina").await;
    note(&begin);
    let pending = pending_of(&begin);
    note(&env.send(env.post(INVITE, &json!({"step": "finish", "credential": {"id": CANARY, "name": CANARY}, "prf": CANARY})).set_cookies(&[(PENDING, &pending)])).await);
    let begin = env.begin_passkey(INVITE, &passkey_token, "gina").await;
    note(&begin);
    let pending = pending_of(&begin);
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    let finish = env.finish_credential(INVITE, &pending, &credential, Some(CANARY)).await;
    note(&finish);
    assert_eq!(finish.status, 200, "{}", finish.text());
    let recovery_two = finish.json()["recovery_code"].as_str().expect("a recovery code").to_owned();

    for (n, text) in seen.iter().enumerate() {
        assert!(!text.contains(CANARY) && !text.contains(&loud), "answer {n} holds the canary");
    }
    let audit = env.audit_text();
    assert!(!audit.contains(CANARY) && !audit.contains(&loud) && !audit.contains("canary-"), "the audit holds the canary");

    // The two secrets of the flow each appear in exactly one answer, and nowhere in the audit.
    let appearing = |needle: &str| seen.iter().filter(|text| text.contains(needle)).count();
    assert_eq!(appearing(&secret_text), 1, "the authenticator secret is in exactly one answer");
    assert_eq!(appearing(&recovery), 1, "the recovery code is in exactly one answer");
    assert_eq!(appearing(&recovery_two), 1, "the second recovery code is in exactly one answer");
    assert!(!audit.contains(&secret_text) && !audit.contains(&recovery) && !audit.contains(&recovery_two));
    assert!(!audit.contains(&session), "the session token is not in the audit");
    for provision in env.gate.provisions() {
        for key in provision.keys {
            let hex: String = key.key.iter().map(|b| format!("{b:02x}")).collect();
            assert!(!audit.contains(&hex) && seen.iter().all(|t| !t.contains(&hex)), "a key is nowhere");
        }
    }
    println!(
        "Canary: {} answers (bodies and every header) and the whole audit directory searched; the canary is nowhere; the recovery code and the authenticator secret are each in exactly one answer",
        seen.len()
    );
}

#[test]
fn canary_the_route_source_has_no_way_to_write_a_secret_down() {
    let source = include_str!("../src/http/routes_enrol.rs");
    for banned in ["println!", "eprintln!", "dbg!", "print!(", "tracing::", "format!(\"{:?}\"", "{:?}"] {
        assert!(!source.contains(banned), "routes_enrol.rs contains {banned}");
    }
    for (n, line) in source.lines().enumerate() {
        if line.contains("log::event(") {
            assert!(line.contains("&[])"), "routes_enrol.rs:{}: a log line may carry no field: {line}", n + 1);
        }
    }
}
