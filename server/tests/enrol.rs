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

#[allow(dead_code)]
fn unused(_: Value) {}
