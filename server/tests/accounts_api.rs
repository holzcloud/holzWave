//! The account and administration routes through the real router: the list of accounts, the invitations, the three gates every
//! route passes (a session, the role, a recent sign-in) and, in the later sections, a person's own account and the changes an
//! administrator makes.
//!
//! The router is called as a `tower` service (no socket). The services behind it are the real ones over a scratch store and a
//! manual clock; the seam to the secret store is a recording fake. See `support/api_env.rs`.

#[path = "support/api_env.rs"]
mod api_env;
#[path = "support/auth_env.rs"]
mod auth_env;
#[path = "support/edge.rs"]
mod edge;

use api_env::{ApiEnv, PUBLIC_URL, SIX_MINUTES};
use auth_env::webauthn_client::{SoftPasskey, Tweaks};
use auth_env::{Methods, PASSWORD, Tuning, WRONG, WithCookies, prf_text, set_cookie_value};
use axum::http::Method;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use edge::{Resp, Signed};
use holzwave_server::accounts::Ending;
use holzwave_server::auth::password::params_text;
use holzwave_server::auth::session::parse_token;
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::http::routes_me::SecretsError;
use holzwave_server::store::{AccountId, AccountState, PasswordCredential, Role, UnlockKind};
use holzwave_server::unlock::{kek_from_passphrase, kek_from_prf, kek_from_recovery, parse_recovery_code};
use serde_json::{Value, json};
use std::sync::Arc;
use webauthn_rs::prelude::Url;

const SESSION: &str = "__Host-hw_session";

/// The audit lines with the action.
fn lines_of(env: &ApiEnv, action: &str) -> Vec<Value> {
    env.audit_lines().into_iter().filter(|l| l["action"] == action).collect()
}

/// The accounts of a list answer by username.
fn by_name<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["accounts"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|a| a["username"] == name)
        .unwrap_or_else(|| panic!("{name} is not in the list: {body}"))
}

fn text_time(unix: i64) -> String {
    rfc3339(from_unix(unix))
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer: the list, an invitation, and the three gates
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn tracer_the_list_shows_every_live_account_with_its_state_and_its_last_sign_in() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let signed_in_at = env.clock.unix();
    env.clock.advance(120);
    env.member("bob");
    let bob_signed_in_at = env.clock.unix();
    env.bare("carol", Role::Member); // never signed in
    env.bare("dora", Role::Member);
    env.set_state("dora", AccountState::Disabled);
    let gone = env.bare("gone", Role::Member);
    env.store.write(|tx| tx.account_delete(gone)).expect("delete");
    env.person("dave").await;
    for _ in 0..5 {
        let refused = env.send(env.post("/auth/password", &json!({"username": "dave", "password": WRONG}))).await;
        assert!(matches!(refused.status.as_u16(), 401 | 429), "{}", refused.text());
    }
    assert!(env.signin.is_locked("dave", env.clock.unix()).expect("read"), "dave is locked out of the password path");

    let response = env.send(env.get_as("/api/admin/accounts", &alice)).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    let body = response.json();
    let mut members: Vec<&str> = body.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["accounts", "admins"]);
    assert_eq!(body["admins"], 1);
    let names: Vec<&str> = body["accounts"].as_array().expect("a list").iter().map(|a| a["username"].as_str().expect("name")).collect();
    assert_eq!(names, ["alice", "bob", "carol", "dora", "dave"], "in the order they were made, the deleted one left out");

    for account in body["accounts"].as_array().expect("a list") {
        let mut keys: Vec<&str> = account.as_object().expect("an object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["created_at", "display_name", "id", "last_signin", "role", "state", "username"]);
    }
    let alice_row = by_name(&body, "alice");
    assert_eq!(alice_row["role"], "admin");
    assert_eq!(alice_row["state"], "active");
    assert_eq!(alice_row["id"], alice.account.get());
    assert_eq!(alice_row["last_signin"], text_time(signed_in_at), "RFC 3339, UTC");
    assert_eq!(by_name(&body, "bob")["last_signin"], text_time(bob_signed_in_at));
    assert_eq!(by_name(&body, "bob")["role"], "member");
    assert_eq!(by_name(&body, "carol")["last_signin"], Value::Null, "never signed in");
    assert_eq!(by_name(&body, "dora")["state"], "disabled");
    assert_eq!(by_name(&body, "dave")["state"], "locked_out");
    assert_eq!(by_name(&body, "carol")["state"], "active");
    assert!(alice_row["created_at"].as_str().expect("a time").ends_with('Z'));
    println!(
        "Tracer: list 200 admins=1, 5 accounts (alice admin active, bob, carol never signed in, dora disabled, dave locked_out), the deleted one is not listed"
    );
}

#[tokio::test]
async fn tracer_an_invitation_link_enrols_a_person_whose_last_sign_in_is_the_redemption() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let response = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &alice)).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    let body = response.json();
    let mut members: Vec<&str> = body.as_object().expect("an object").keys().map(String::as_str).collect();
    members.sort_unstable();
    assert_eq!(members, ["expires_at", "link"]);
    let link = body["link"].as_str().expect("a link");
    let token = link.strip_prefix(&format!("{PUBLIC_URL}/#invite=")).unwrap_or_else(|| panic!("not the link shape: {link}"));
    assert!(token.starts_with("hwi_") && token.len() == 4 + 43, "{token}");
    assert_eq!(body["expires_at"], text_time(env.clock.unix() + 7 * 86_400), "seven days is the default");
    assert_eq!(env.invitation_rows(), 1);

    let audit = lines_of(&env, "account.invite");
    assert_eq!(audit.len(), 1, "one audit line");
    assert_eq!(audit[0]["result"], "ok");
    assert_eq!(audit[0]["actor"], alice.account.get().to_string());
    assert!(!env.audit_text().contains(token), "the token is in no audit line");

    // The link is good: the person enrols through it, and the sign-in is stamped.
    env.clock.advance(90);
    let (finish, _secret) = env.redeem_with_password(token, "bob").await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    let bob = env.store.account_by_username("bob").expect("read").expect("bob exists");
    assert_eq!(bob.role, Role::Member);
    assert_eq!(env.last_signin(bob.id), Some(env.clock.unix()), "the stamp is the time of the redemption");
    assert!(set_cookie_value(&finish, SESSION).is_some());
    println!("Tracer: invitation 200 link {PUBLIC_URL}/#invite=hwi_<43>, expires in 7 days, 1 audit line, bob redeemed it and has last_signin");
}

#[tokio::test]
async fn tracer_gate_a_member_is_refused_the_list_and_the_invitation_without_a_step_up_hint() {
    let env = ApiEnv::new();
    let bob = env.member("bob");
    let list = env.send(env.get_as("/api/admin/accounts", &bob)).await;
    assert_eq!(list.status, 403, "{}", list.text());
    assert_eq!(list.json(), json!({"code": "forbidden"}), "no step_up: a recent sign-in would not help");
    let invite = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &bob)).await;
    assert_eq!(invite.status, 403, "{}", invite.text());
    assert_eq!(invite.json(), json!({"code": "forbidden"}));
    assert_eq!(env.invitation_rows(), 0);
    assert!(lines_of(&env, "account.invite").is_empty(), "nothing was written down");
    println!("Gates: member GET 403 {{code: forbidden}} without step_up, member POST 403 the same, 0 invitations, 0 audit lines");
}

#[tokio::test]
async fn tracer_gate_without_a_session_the_answer_is_401_invalid() {
    let env = ApiEnv::new();
    let list = env.send(env.get("/api/admin/accounts")).await;
    assert_eq!(list.status, 401, "{}", list.text());
    assert_eq!(list.json(), json!({"code": "invalid"}));
    let invite = env.send(env.post("/api/admin/invitations", &json!({"role": "member"}))).await;
    assert_eq!(invite.status, 401);
    assert_eq!(invite.json(), json!({"code": "invalid"}));
    assert_eq!(env.invitation_rows(), 0);
    println!("Gates: no cookie GET 401 invalid, POST 401 invalid");
}

#[tokio::test]
async fn tracer_gate_an_old_sign_in_may_read_but_not_change_and_nothing_is_written() {
    let env = ApiEnv::new();
    let alice = env.admin_stale("alice");
    assert_eq!(env.clock.unix() - alice.session.last_auth, SIX_MINUTES);

    let read = env.send(env.get_as("/api/admin/accounts", &alice)).await;
    assert_eq!(read.status, 200, "a read does not need a recent sign-in: {}", read.text());

    let invite = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &alice)).await;
    assert_eq!(invite.status, 403, "{}", invite.text());
    assert_eq!(invite.json(), json!({"code": "forbidden", "step_up": true}));
    assert_eq!(env.invitation_rows(), 0, "no invitation row");
    assert!(env.audit_lines().is_empty(), "no audit line at all: {:?}", env.audit_lines());
    println!("Gates: stale admin GET 200, POST 403 {{code: forbidden, step_up: true}}, 0 invitation rows, 0 audit lines");
}

// ---------------------------------------------------------------------------------------------------------------------
// Helpers of the person's own routes
// ---------------------------------------------------------------------------------------------------------------------

const NEW_PASSWORD: &str = "a different horse battery";
const HKDF: &str = "hkdf-sha256";

fn keys_of(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().expect("an object").keys().cloned().collect();
    keys.sort_unstable();
    keys
}

fn assert_no_content(response: &Resp, what: &str) {
    assert_eq!(response.status, 204, "{what}: {}", response.text());
    assert!(response.body.is_empty(), "{what}: a body on a 204");
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
}

fn assert_code(response: &Resp, status: u16, body: Value, what: &str) {
    assert_eq!(response.status.as_u16(), status, "{what}: {}", response.text());
    assert_eq!(response.json(), body, "{what}");
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"), "{what}");
}

/// The password and the code of a sign-in through the router; answers the key the gate was handed, or `None` when a step
/// was refused. The clock moves on by 31 seconds, so that the next code is a new one.
async fn sign_in_key(env: &ApiEnv, name: &str, password: &str, secret: Option<&[u8]>) -> Option<Vec<u8>> {
    let first = env.send(env.post("/auth/password", &json!({"username": name, "password": password}))).await;
    if first.status != 200 {
        return None;
    }
    let pending = set_cookie_value(&first, "__Host-hw_pending").expect("a pending cookie");
    let code = match secret {
        Some(secret) => totp::code_at(secret, env.clock.unix()).expect("a code").to_string(),
        None => env.code(name),
    };
    let second =
        env.send(env.post("/auth/totp", &json!({"code": code})).set_cookies(&[("__Host-hw_pending", &pending)])).await;
    env.clock.advance(31);
    if second.status != 200 {
        return None;
    }
    env.gate.started().last().map(|call| call.key.clone())
}

/// The status of the first step of a password sign-in.
async fn password_status(env: &ApiEnv, name: &str, password: &str) -> u16 {
    env.send(env.post("/auth/password", &json!({"username": name, "password": password}))).await.status.as_u16()
}

fn action_lines(env: &ApiEnv, action: &str) -> Vec<Value> {
    lines_of(env, action)
}

/// A password row that is not a real credential, so that a test can give many accounts one without a derivation each.
fn plant_password(env: &ApiEnv, id: AccountId) {
    let credential = PasswordCredential {
        verifier: vec![9; 32],
        salt: vec![8; 16],
        params: params_text(&env.argon.kdf()),
        changed_at: env.clock.unix(),
    };
    env.store.write(|tx| tx.password_set(id, &credential)).expect("a password row");
}

/// A code that none of the three steps of the window accepts.
fn wrong_code_for(env: &ApiEnv, secret: &[u8]) -> String {
    let now = env.clock.unix();
    let accepted: Vec<String> =
        [-30i64, 0, 30].iter().map(|d| totp::code_at(secret, now + d).expect("a code").to_string()).collect();
    (0..1_000_000).map(|n| format!("{n:06}")).find(|c| !accepted.contains(c)).expect("a code")
}

fn secret_of(begin: &Resp) -> Vec<u8> {
    let text = begin.json()["secret"].as_str().expect("a secret").to_owned();
    totp_rs::Secret::try_from_base32(text).expect("base32").as_bytes().to_vec()
}

// ---------------------------------------------------------------------------------------------------------------------
// GET /api/me
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn me_answers_the_shape_of_the_contract_for_a_person_with_a_password() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let response = env.me_of(&bob).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    let body = response.json();
    assert_eq!(keys_of(&body), ["account", "oidc", "passkeys", "password", "secrets", "sessions", "totp"]);
    assert_eq!(keys_of(&body["account"]), ["display_name", "id", "role", "username"]);
    assert_eq!(body["account"], json!({"id": bob.account.get(), "username": "bob", "display_name": "bob", "role": "member"}));
    assert_eq!(body["oidc"], Value::Null);
    assert_eq!(body["password"], true);
    assert_eq!(body["totp"], true);
    assert_eq!(body["passkeys"], json!([]));
    assert_eq!(body["secrets"], json!({"state": "unlocked", "passphrase": false, "recovery": false}));
    let sessions = body["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(keys_of(&sessions[0]), ["addr", "created_at", "current", "device", "id", "last_seen"]);
    assert_eq!(sessions[0]["current"], true);
    assert_eq!(sessions[0]["device"], "Firefox on Linux");
    assert_eq!(sessions[0]["addr"], "192.0.2.7");
    assert_eq!(sessions[0]["created_at"], text_time(env.clock.unix()));
    let text = response.text();
    assert!(!text.contains(&bob.cookie), "the cookie value is in no answer");
    assert!(!text.contains(&bob.csrf), "nor is the anti-forgery token");
    println!("Me: GET /api/me 200 account, oidc null, password true, totp true, passkeys [], secrets {{unlocked, false, false}}, sessions 1 current");
}

#[tokio::test]
async fn me_needs_a_session_and_a_read_needs_no_recent_sign_in() {
    let env = ApiEnv::new();
    let anonymous = env.send(env.get("/api/me")).await;
    assert_code(&anonymous, 401, json!({"code": "invalid"}), "no cookie");
    let stale = env.stale_member_with_password("bob").await;
    let read = env.me_of(&stale).await;
    assert_eq!(read.status, 200, "a read does not need a recent sign-in: {}", read.text());
    let post = env.send(env.post_json("/api/me/recovery", &json!({}), &stale)).await;
    assert_code(&post, 403, json!({"code": "forbidden", "step_up": true}), "a change after an old sign-in");
}

#[tokio::test]
async fn me_lists_the_passkeys_without_a_credential_byte() {
    let env = ApiEnv::new();
    let (pk, key) = env.member_with_passkey("pk");
    let response = env.me_of(&pk).await;
    let body = response.json();
    let list = body["passkeys"].as_array().expect("passkeys");
    assert_eq!(list.len(), 1);
    assert_eq!(keys_of(&list[0]), ["created_at", "id", "last_used", "name"]);
    assert_eq!(list[0]["name"], "Key");
    assert_eq!(list[0]["created_at"], text_time(env.clock.unix()));
    assert_eq!(list[0]["last_used"], Value::Null);
    assert!(list[0]["id"].is_number());
    assert_eq!(body["password"], false);
    assert_eq!(body["totp"], false);
    let credential = URL_SAFE_NO_PAD.encode(key.credential_id());
    assert!(!response.text().contains(&credential), "the credential id is not shown");
}

#[tokio::test]
async fn me_names_each_session_by_a_handle_that_is_not_the_cookie_and_marks_the_current_one() {
    let env = ApiEnv::new();
    let first = env.member("bob");
    env.clock.advance(5);
    let second = env.another_session("bob");
    let response = env.me_of(&second).await;
    let body = response.json();
    let sessions = body["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 2);
    let expected = |who: &Signed| {
        let hash = parse_token(&who.cookie).expect("a token");
        URL_SAFE_NO_PAD.encode(&hash[..12])
    };
    assert_eq!(sessions[0]["id"], expected(&first), "oldest first");
    assert_eq!(sessions[1]["id"], expected(&second));
    assert_eq!(sessions[0]["current"], false);
    assert_eq!(sessions[1]["current"], true);
    for session in sessions {
        let id = session["id"].as_str().expect("a handle");
        assert_eq!(id.len(), 16, "12 bytes in 16 characters");
        assert!(!first.cookie.contains(id) && !second.cookie.contains(id), "no part of a cookie value");
    }
    // The handle is not a cookie.
    let as_cookie = env.send(env.get("/api/me").cookie(sessions[1]["id"].as_str().expect("a handle"))).await;
    assert_eq!(as_cookie.status, 401);
    println!("Me: two sessions named by 16-character handles (first 12 bytes of the stored hash), the current one marked, handle as cookie 401");
}

#[tokio::test]
async fn me_does_not_list_a_session_that_is_past_its_idle_time() {
    let env = ApiEnv::new();
    env.member("bob");
    env.clock.advance(1_900);
    let fresh = env.another_session("bob");
    let body = env.me_of(&fresh).await.json();
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 1, "the idle session is not shown");
}

#[tokio::test]
async fn me_tells_a_locked_store_and_the_unlock_methods_the_person_has() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    env.store
        .write(|tx| {
            tx.unlock_add(bob.account, UnlockKind::Passphrase, &[1; 16], "{}", None)?;
            tx.unlock_add(bob.account, UnlockKind::Recovery, &[2; 16], HKDF, None)
        })
        .expect("unlock rows");
    env.gate.set_locked(bob.account, true);
    let body = env.me_of(&bob).await.json();
    assert_eq!(body["secrets"], json!({"state": "locked", "passphrase": true, "recovery": true}));
    env.gate.set_locked(bob.account, false);
    assert_eq!(env.me_of(&bob).await.json()["secrets"]["state"], "unlocked");
}

#[tokio::test]
async fn me_names_the_provider_of_a_linked_identity() {
    let env = ApiEnv::build(Methods::ALL, Tuning::default());
    let linked = env.member("lin");
    env.member("plain");
    env.store
        .write(|tx| tx.oidc_link(linked.account, auth_env::oidc_provider::ISSUER, "subject-1"))
        .expect("a link");
    let body = env.me_of(&linked).await.json();
    assert_eq!(body["oidc"], json!({"id": "corp", "name": "Company"}));
    assert_eq!(body["password"], false);
    let plain = env.another_session("plain");
    assert_eq!(env.me_of(&plain).await.json()["oidc"], Value::Null);
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/password
// ---------------------------------------------------------------------------------------------------------------------

fn change_body(current: &str, new: &str) -> Value {
    json!({"current": current, "new": new})
}

#[tokio::test]
async fn password_a_change_re_wraps_the_slot_with_the_old_key_and_stores_the_new_password() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let secret_before = env.credential_dump(bob.account);
    let old_key = sign_in_key(&env, "bob", PASSWORD, None).await.expect("the old password signs in");

    let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
    assert_no_content(&response, "the change");
    assert!(response.all("set-cookie").is_empty(), "no cookie");

    // The seam was asked once: the new key, opened by the old one.
    let sets = env.secrets.sets();
    assert_eq!(sets.len(), 1, "{sets:?}");
    let (account, key, via) = &sets[0];
    assert_eq!(*account, bob.account);
    assert_eq!((key.kind, key.credential_id.as_ref()), (UnlockKind::Password, None));
    let via = via.as_ref().expect("the old key opens the store");
    assert_eq!((via.kind, via.credential_id.as_ref()), (UnlockKind::Password, None));
    assert_eq!(via.key, old_key, "the old key is the one the old password gives");
    assert_ne!(key.key, via.key);

    // The old password is gone, the new one signs in, and it gives exactly the key the seam got.
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 401);
    let new_key = sign_in_key(&env, "bob", NEW_PASSWORD, None).await.expect("the new password signs in");
    assert_eq!(new_key, key.key, "the key of the new password is the key the slot was set to");
    assert_ne!(secret_before, env.credential_dump(bob.account));

    // The unlock row holds the new salt; there is still one password row of the account.
    let rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    let password_rows: Vec<_> = rows.iter().filter(|m| m.kind == UnlockKind::Password).collect();
    assert!(password_rows.len() <= 1);

    let lines = action_lines(&env, "auth.password.change");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], bob.account.get().to_string());
    assert_eq!(lines[0]["target"], bob.account.get().to_string());
    assert!(!env.audit_text().contains(NEW_PASSWORD) && !env.audit_text().contains(PASSWORD), "no password in the audit file");
    println!("Password: 204, seam set_slot(Password, new key, via old key) once, new password gives the key, old 401, 1 audit line auth.password.change");
}

#[tokio::test]
async fn password_a_change_writes_the_password_unlock_row_with_the_new_salt() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    // The row an enrolment writes for the password.
    env.store
        .write(|tx| tx.unlock_add(bob.account, UnlockKind::Password, &[5; 16], &params_text(&env.argon.kdf()), None))
        .expect("an unlock row");
    let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
    assert_no_content(&response, "the change");
    let credential = env.store.read(|tx| tx.password_get(bob.account)).expect("read").expect("a password row");
    let rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_eq!(rows.len(), 1, "replaced, not added: {rows:?}");
    assert_eq!(rows[0].kind, UnlockKind::Password);
    assert_eq!(rows[0].salt, credential.salt, "the unlock row holds the salt of the new password");
    assert_ne!(rows[0].salt, vec![5; 16].as_slice());
    assert_eq!(rows[0].params, credential.params);
}

#[tokio::test]
async fn password_a_wrong_current_password_is_invalid_and_changes_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let last_auth = bob.session.last_auth;
    env.clock.advance(60);

    let response = env.send(env.post_json("/api/me/password", &change_body(WRONG, NEW_PASSWORD), &bob)).await;
    assert_code(&response, 401, json!({"code": "invalid"}), "a wrong current password");
    assert!(response.all("set-cookie").is_empty());
    assert!(response.header("retry-after").is_none());
    assert_eq!(env.credential_dump(bob.account), before);
    assert!(env.secrets.calls().is_empty(), "the seam was not asked");
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 200, "the old password still works");

    // One failed line, about the change step; no step-up line; the session is as it was.
    let fails: Vec<Value> = env.audit_lines().into_iter().filter(|l| l["action"] == "signin.fail").collect();
    assert_eq!(fails.len(), 1, "{fails:?}");
    assert_eq!(fails[0]["detail"]["step"], "change");
    assert_eq!(fails[0]["actor"], bob.account.get().to_string());
    assert_eq!(env.count("session.step_up"), 0);
    assert_eq!(env.count("auth.password.change"), 0);
    let session = env.store.read(|tx| tx.session_get(&bob.session.id_hash)).expect("read");
    assert_eq!(session.expect("the session").last_auth, last_auth, "a check of the password is not a step-up");
}

#[tokio::test]
async fn password_wrong_current_passwords_count_toward_the_lock_of_the_sign_in() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    for n in 0..5 {
        let response = env.send(env.post_json("/api/me/password", &change_body(WRONG, NEW_PASSWORD), &bob)).await;
        assert_eq!(response.status, 401, "attempt {n}: {}", response.text());
    }
    let locked = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
    assert_eq!(locked.status, 429, "{}", locked.text());
    assert_eq!(locked.json()["code"], "throttled");
    let seconds = locked.json()["retry_seconds"].as_u64().expect("seconds");
    assert!((1..=900).contains(&seconds), "{seconds}");
    assert_eq!(locked.header("retry-after").as_deref(), Some(seconds.to_string().as_str()));
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 429, "the same lock holds the sign-in");
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.count("auth.password.change"), 0);
}

#[tokio::test]
async fn password_a_new_password_that_breaks_the_policy_is_a_400_on_new_before_anything_is_written() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob.the.builder").await;
    let before = env.credential_dump(bob.account);
    let too_long = "ab".repeat(129);
    let cases: [(&str, &str); 4] = [
        ("too short", "short pass"),
        ("the username", "BOB.THE.BUILDER"),
        ("one repeated character", "aaaaaaaaaaaaaaaa"),
        ("too long", too_long.as_str()),
    ];
    for (what, new) in cases {
        let derivations = env.argon.derivations();
        let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, new), &bob)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "new"}), what);
        assert_eq!(env.argon.derivations() - derivations, 1, "{what}: only the current password was derived");
        assert_eq!(env.credential_dump(bob.account), before, "{what}");
    }
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.count("auth.password.change"), 0);
}

#[tokio::test]
async fn password_an_account_without_a_password_is_not_found_and_nothing_is_derived() {
    let env = ApiEnv::new();
    let (pk, _key) = env.member_with_passkey("pk");
    let derivations = env.argon.derivations();
    let lines = env.audit_lines().len();
    let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &pk)).await;
    assert_code(&response, 404, json!({"code": "not_found"}), "no password");
    assert_eq!(env.argon.derivations(), derivations);
    assert_eq!(env.audit_lines().len(), lines, "nothing was written down");
}

#[tokio::test]
async fn password_a_seam_that_cannot_do_it_changes_nothing_and_says_why() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    for (error, status, body) in [
        (SecretsError::Unavailable, 503, json!({"code": "busy"})),
        (SecretsError::Locked, 423, json!({"code": "locked"})),
        (SecretsError::Refused, 409, json!({"code": "conflict", "field": "method"})),
    ] {
        env.secrets.fail_set(Some(error));
        let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
        assert_code(&response, status, body, &format!("{error:?}"));
        assert_eq!(env.credential_dump(bob.account), before, "{error:?}: the store is as it was");
    }
    env.secrets.fail_set(None);
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 200, "the old password still works");
    assert_eq!(password_status(&env, "bob", NEW_PASSWORD).await, 401);
    assert_eq!(env.count("auth.password.change"), 0, "no success was written down");
    assert!(env.secrets.removes().is_empty());
}

#[tokio::test]
async fn rollback_a_password_change_whose_row_cannot_be_written_puts_the_old_key_back() {
    for (label, event, table, when) in [
        ("the password row", "UPDATE", "password_credentials", ""),
        ("the unlock row", "INSERT", "unlock_methods", ""),
        ("the unlock row replaced", "DELETE", "unlock_methods", ""),
    ] {
        let env = ApiEnv::new();
        let bob = env.member_with_password("bob").await;
        env.store
            .write(|tx| tx.unlock_add(bob.account, UnlockKind::Password, &[5; 16], &params_text(&env.argon.kdf()), None))
            .expect("an unlock row");
        let old_key = sign_in_key(&env, "bob", PASSWORD, None).await.expect("signs in");
        let before = env.credential_dump(bob.account);
        env.break_writes("hw_broken", event, table, when);
        // A password row may be replaced by an insert that conflicts: break both ways for the password table.
        if table == "password_credentials" {
            env.break_writes("hw_broken_insert", "INSERT", table, when);
        }

        let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
        assert_code(&response, 503, json!({"code": "busy"}), label);
        assert_eq!(env.credential_dump(bob.account), before, "{label}: one transaction, nothing kept");

        let sets = env.secrets.sets();
        assert_eq!(sets.len(), 2, "{label}: the slot was set, then set back: {sets:?}");
        let (_, new_key, via_old) = (&sets[0].0, &sets[0].1, sets[0].2.as_ref().expect("via"));
        assert_eq!(via_old.key, old_key, "{label}");
        let back = &sets[1];
        assert_eq!(back.1.key, old_key, "{label}: the old key is put back");
        assert_eq!(back.2.as_ref().expect("via").key, new_key.key, "{label}: opened by the new one");

        env.repair("hw_broken");
        if table == "password_credentials" {
            env.repair("hw_broken_insert");
        }
        assert_eq!(password_status(&env, "bob", PASSWORD).await, 200, "{label}: the old password still works");
        assert_eq!(env.count("auth.password.change"), 0, "{label}");
    }
}

#[tokio::test]
async fn password_a_body_that_is_not_the_two_strings_names_the_member_and_derives_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("an empty object", json!({}), Some("current")),
        ("no new", json!({"current": PASSWORD}), Some("new")),
        ("no current", json!({"new": NEW_PASSWORD}), Some("current")),
        ("a number as current", json!({"current": 1, "new": NEW_PASSWORD}), Some("current")),
        ("null as new", json!({"current": PASSWORD, "new": null}), Some("new")),
        ("an object as new", json!({"current": PASSWORD, "new": {"a": 1}}), Some("new")),
        ("an unknown member", json!({"current": PASSWORD, "new": NEW_PASSWORD, "extra": 1}), None),
        ("an array", json!([PASSWORD, NEW_PASSWORD]), None),
        ("a string", json!("x"), None),
    ];
    for (what, body, field) in cases {
        let derivations = env.argon.derivations();
        let response = env.send(env.post_json("/api/me/password", &body, &bob)).await;
        let expected = match field {
            Some(field) => json!({"code": "bad_request", "field": field}),
            None => json!({"code": "bad_request"}),
        };
        assert_code(&response, 400, expected, what);
        assert_eq!(env.argon.derivations(), derivations, "{what}: nothing was derived");
    }
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.count("signin.fail"), 0, "a malformed body is not a wrong password");
}

#[tokio::test]
async fn password_a_change_ends_no_session_and_moves_no_sign_in_time() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let other = env.another_session("bob");
    env.clock.advance(20);
    let response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
    assert_no_content(&response, "the change");
    assert_eq!(env.session_rows(bob.account), 2);
    assert_eq!(env.me_of(&other).await.status, 200, "the other session still works");
    let rows = env.store.read(|tx| tx.session_list(bob.account)).expect("rows");
    assert!(rows.iter().all(|s| s.last_auth == bob.session.last_auth), "no step-up was recorded");
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/totp
// ---------------------------------------------------------------------------------------------------------------------

async fn totp_begin(env: &ApiEnv, who: &Signed) -> Resp {
    env.send(env.post_json("/api/me/totp", &json!({"step": "begin"}), who)).await
}

async fn totp_finish(env: &ApiEnv, who: &Signed, code: &str) -> Resp {
    env.send(env.post_json("/api/me/totp", &json!({"step": "finish", "code": code}), who)).await
}

#[tokio::test]
async fn totp_begin_shows_a_new_secret_and_changes_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let begin = totp_begin(&env, &bob).await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    assert_eq!(begin.header("cache-control").as_deref(), Some("no-store"));
    let body = begin.json();
    assert_eq!(keys_of(&body), ["otpauth", "secret"]);
    let secret = body["secret"].as_str().expect("a secret");
    assert_eq!(secret.len(), 32, "20 bytes in base32");
    assert!(secret.bytes().all(|b| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b)));
    let uri = body["otpauth"].as_str().expect("a link");
    assert!(uri.starts_with("otpauth://totp/hw.example.test:bob?"), "{uri}");
    assert!(uri.contains(&format!("secret={secret}")));
    assert_eq!(env.credential_dump(bob.account), before, "the old authenticator stays");
    assert_eq!(env.me.pending_count(), 1);
    assert!(env.audit_lines().is_empty(), "nothing is written down by a begin");
    assert!(sign_in_key(&env, "bob", PASSWORD, None).await.is_some(), "the old authenticator still signs in");
    println!("Totp: begin 200 {{secret(32), otpauth}}, store unchanged, 1 pending, 0 audit lines");
}

#[tokio::test]
async fn totp_finish_with_the_right_code_replaces_the_authenticator_and_spends_the_step() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let begin = totp_begin(&env, &bob).await;
    let secret = secret_of(&begin);
    let step = totp::step_of(env.clock.unix());
    let code = totp::code_at(&secret, env.clock.unix()).expect("a code").to_string();

    let finish = totp_finish(&env, &bob, &code).await;
    assert_no_content(&finish, "the finish");
    assert_eq!(env.me.pending_count(), 0);
    let row = env.store.read(|tx| tx.totp_get(bob.account)).expect("read").expect("a row");
    assert_eq!(row.last_step, step, "the confirmed step is spent");

    // The code that was just shown to work cannot sign in a second time; the next one can, and the old app cannot.
    let first = env.send(env.post("/auth/password", &json!({"username": "bob", "password": PASSWORD}))).await;
    let pending = set_cookie_value(&first, "__Host-hw_pending").expect("pending");
    let replay = env
        .send(env.post("/auth/totp", &json!({"code": code})).set_cookies(&[("__Host-hw_pending", &pending)]))
        .await;
    assert_eq!(replay.status, 401, "the confirmed step cannot be used again");
    env.clock.advance(31);
    assert!(sign_in_key(&env, "bob", PASSWORD, Some(&secret)).await.is_some(), "the new authenticator signs in");
    assert!(sign_in_key(&env, "bob", PASSWORD, None).await.is_none(), "the old authenticator does not");

    let lines = action_lines(&env, "auth.totp.enrol");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], bob.account.get().to_string());
    assert!(!env.audit_text().contains(&code));
}

#[tokio::test]
async fn totp_a_wrong_code_keeps_the_old_authenticator_and_the_third_one_ends_the_attempt() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let begin = totp_begin(&env, &bob).await;
    let secret = secret_of(&begin);
    let wrong = wrong_code_for(&env, &secret);
    for attempt in 1..=3 {
        let response = totp_finish(&env, &bob, &wrong).await;
        assert_code(&response, 401, json!({"code": "invalid"}), &format!("wrong code {attempt}"));
        assert_eq!(env.credential_dump(bob.account), before);
    }
    assert_eq!(env.me.pending_count(), 0, "three wrong codes drop the attempt");
    let right = totp::code_at(&secret, env.clock.unix()).expect("a code").to_string();
    let late = totp_finish(&env, &bob, &right).await;
    assert_code(&late, 401, json!({"code": "invalid"}), "the right code after the attempt ended");
    assert_eq!(env.credential_dump(bob.account), before);
    assert_eq!(env.count("auth.totp.enrol"), 0);
}

#[tokio::test]
async fn totp_a_second_begin_replaces_the_first_and_only_one_attempt_exists_per_person() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let first = secret_of(&totp_begin(&env, &bob).await);
    let second = secret_of(&totp_begin(&env, &bob).await);
    assert_ne!(first, second);
    assert_eq!(env.me.pending_count(), 1);
    let stale = totp::code_at(&first, env.clock.unix()).expect("a code").to_string();
    assert_eq!(totp_finish(&env, &bob, &stale).await.status, 401, "the first secret is gone");
    let right = totp::code_at(&second, env.clock.unix()).expect("a code").to_string();
    assert_no_content(&totp_finish(&env, &bob, &right).await, "the second secret");
}

#[tokio::test]
async fn totp_a_finish_without_a_begin_or_after_five_minutes_is_invalid() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let nothing = totp_finish(&env, &bob, "123456").await;
    assert_code(&nothing, 401, json!({"code": "invalid"}), "no begin");

    let secret = secret_of(&totp_begin(&env, &bob).await);
    env.clock.advance(301);
    let again = env.another_session("bob");
    let code = totp::code_at(&secret, env.clock.unix()).expect("a code").to_string();
    let late = totp_finish(&env, &again, &code).await;
    assert_code(&late, 401, json!({"code": "invalid"}), "five minutes later");
    assert_eq!(env.count("auth.totp.enrol"), 0);
}

#[tokio::test]
async fn totp_an_account_without_a_password_is_not_found() {
    let env = ApiEnv::new();
    let (pk, _key) = env.member_with_passkey("pk");
    assert_code(&totp_begin(&env, &pk).await, 404, json!({"code": "not_found"}), "begin");
    assert_code(&totp_finish(&env, &pk, "123456").await, 404, json!({"code": "not_found"}), "finish");
    assert_eq!(env.me.pending_count(), 0);
}

#[tokio::test]
async fn totp_a_body_of_the_wrong_shape_is_a_400_and_costs_no_attempt() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let secret = secret_of(&totp_begin(&env, &bob).await);
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        ("no step", json!({}), Some("step")),
        ("another step", json!({"step": "peek"}), Some("step")),
        ("a number as step", json!({"step": 1}), Some("step")),
        ("finish without a code", json!({"step": "finish"}), Some("code")),
        ("a number as code", json!({"step": "finish", "code": 123456}), Some("code")),
        ("begin with a code", json!({"step": "begin", "code": "123456"}), None),
        ("an unknown member", json!({"step": "begin", "x": 1}), None),
    ];
    for (what, body, field) in cases {
        let response = env.send(env.post_json("/api/me/totp", &body, &bob)).await;
        let expected = match field {
            Some(field) => json!({"code": "bad_request", "field": field}),
            None => json!({"code": "bad_request"}),
        };
        assert_code(&response, 400, expected, what);
    }
    let right = totp::code_at(&secret, env.clock.unix()).expect("a code").to_string();
    assert_no_content(&totp_finish(&env, &bob, &right).await, "the attempt is still there");
}

#[tokio::test]
async fn totp_the_open_attempts_are_bounded() {
    assert_eq!(holzwave_server::http::routes_me::MAX_PENDING, 1_024, "the bound of the server");
    let env = ApiEnv::build_with(Methods { passkey: true, password: true, oidc: false }, Tuning::default(), 6);
    let mut people = Vec::new();
    for n in 0..7 {
        let who = env.member(&format!("p{n}"));
        plant_password(&env, who.account);
        people.push(who);
    }
    for who in &people[..6] {
        let response = totp_begin(&env, who).await;
        assert_eq!(response.status, 200, "{}", response.text());
    }
    assert_eq!(env.me.pending_count(), 6);
    let full = totp_begin(&env, &people[6]).await;
    assert_code(&full, 503, json!({"code": "busy"}), "the seventh attempt");
    // A person who has an attempt may start it again: it replaces the first.
    assert_eq!(totp_begin(&env, &people[0]).await.status, 200);
    assert_eq!(env.me.pending_count(), 6);
    // Time makes room.
    env.clock.advance(301);
    let late = env.another_session("p6");
    assert_eq!(totp_begin(&env, &late).await.status, 200);
    assert_eq!(env.me.pending_count(), 1);
    println!("Totp: the open attempts are bounded (the bound of the server is 1,024), the next one 503 busy, a restart replaces, expiry makes room");
}

// ---------------------------------------------------------------------------------------------------------------------
// POST /api/me/passkeys and DELETE /api/me/passkeys/{id}
// ---------------------------------------------------------------------------------------------------------------------

async fn passkey_begin(env: &ApiEnv, who: &Signed, name: &str) -> Resp {
    env.send(env.post_json("/api/me/passkeys", &json!({"step": "begin", "name": name}), who)).await
}

async fn passkey_finish(env: &ApiEnv, who: &Signed, credential: &Value, prf: Option<&str>) -> Resp {
    let mut body = json!({"step": "finish", "credential": credential});
    if let Some(prf) = prf {
        body["prf"] = json!(prf);
    }
    env.send(env.post_json("/api/me/passkeys", &body, who)).await
}

/// A whole registration through the routes with a new software authenticator.
async fn add_passkey(env: &ApiEnv, who: &Signed, name: &str, prf: Option<&str>) -> (Resp, SoftPasskey) {
    let begin = passkey_begin(env, who, name).await;
    assert_eq!(begin.status, 200, "begin: {}", begin.text());
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("the authenticator answers");
    (passkey_finish(env, who, &credential, prf).await, key)
}

fn passkey_ids(env: &ApiEnv, id: AccountId) -> Vec<i64> {
    env.store.read(|tx| tx.passkey_list(id)).expect("list").into_iter().map(|p| p.id).collect()
}

#[tokio::test]
async fn passkey_begin_answers_creation_options_that_name_the_person_and_changes_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let begin = passkey_begin(&env, &bob, "Laptop").await;
    assert_eq!(begin.status, 200, "{}", begin.text());
    assert_eq!(begin.header("cache-control").as_deref(), Some("no-store"));
    let body = begin.json();
    assert_eq!(keys_of(&body), ["options"]);
    assert_eq!(body["options"]["publicKey"]["user"]["name"], "bob");
    assert_eq!(body["options"]["publicKey"]["rp"]["id"], "hw.example.test");
    assert!(body["options"]["publicKey"]["extensions"]["prf"].is_object(), "the PRF request is in the options");
    assert!(begin.all("set-cookie").is_empty(), "the challenge is kept on the server, not in a cookie");
    assert_eq!(env.credential_dump(bob.account), before);
    assert_eq!(env.me.pending_count(), 1);
    assert!(env.audit_lines().is_empty());
}

#[tokio::test]
async fn passkey_a_registration_with_prf_adds_the_passkey_and_a_prf_slot() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let (finish, key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert_eq!(finish.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(keys_of(&finish.json()), ["id"]);

    let rows = env.store.read(|tx| tx.passkey_list(bob.account)).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(finish.json()["id"], rows[0].id);
    assert_eq!(rows[0].credential_id, key.credential_id());
    let salt = rows[0].prf_salt.clone().expect("a salt");

    // A PRF unlock row for that credential, and the slot of the seam set from the same key.
    let unlock = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_eq!(unlock.len(), 1);
    assert_eq!(unlock[0].kind, UnlockKind::Prf);
    assert_eq!(unlock[0].credential_id.as_deref(), Some(key.credential_id()));
    assert_eq!(unlock[0].salt, salt);
    assert_eq!(unlock[0].params, HKDF);
    let sets = env.secrets.sets();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!((sets[0].0, sets[0].1.kind), (bob.account, UnlockKind::Prf));
    assert_eq!(sets[0].1.credential_id.as_deref(), Some(key.credential_id()));
    assert!(sets[0].2.is_none(), "an open store needs no old key");
    let output: [u8; 32] = [7; 32];
    let expected = kek_from_prf(&output, &salt).expect("a key");
    assert_eq!(sets[0].1.key, expected.expose().to_vec());

    let lines = action_lines(&env, "auth.passkey.add");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], bob.account.get().to_string());
    let me = env.me_of(&bob).await.json();
    assert_eq!(me["passkeys"][0]["name"], "Laptop");
    println!("Passkeys: begin options, finish 200 {{id}}, passkey row, prf unlock row (hkdf-sha256), seam set_slot(Prf, credential), 1 audit line");
}

#[tokio::test]
async fn passkey_a_registration_without_prf_output_adds_no_slot() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let (finish, _key) = add_passkey(&env, &bob, "Phone", None).await;
    assert_eq!(finish.status, 200, "{}", finish.text());
    assert_eq!(passkey_ids(&env, bob.account).len(), 1);
    assert!(env.unlock_kinds(bob.account).is_empty());
    assert!(env.secrets.calls().is_empty());
    // A PRF value that is not 32 bytes is no PRF output.
    let (again, _) = add_passkey(&env, &bob, "Tablet", Some("short")).await;
    assert_eq!(again.status, 200, "{}", again.text());
    assert!(env.secrets.calls().is_empty());
    assert_eq!(passkey_ids(&env, bob.account).len(), 2);
}

#[tokio::test]
async fn passkey_a_seam_that_fails_does_not_undo_the_passkey() {
    for error in [SecretsError::Unavailable, SecretsError::Locked, SecretsError::Refused] {
        let env = ApiEnv::new();
        let bob = env.member_with_password("bob").await;
        env.secrets.fail_set(Some(error));
        let (finish, _key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
        assert_eq!(finish.status, 200, "{error:?}: {}", finish.text());
        assert_eq!(passkey_ids(&env, bob.account).len(), 1, "{error:?}: the passkey exists");
        assert!(env.unlock_kinds(bob.account).is_empty(), "{error:?}: a slot that was not set has no row");
        assert_eq!(env.count("auth.passkey.add"), 1);
    }
}

#[tokio::test]
async fn passkey_an_unlock_row_that_cannot_be_written_takes_the_slot_back() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    env.break_writes("hw_broken", "INSERT", "unlock_methods", "");
    let (finish, key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
    assert_eq!(finish.status, 200, "the passkey itself is registered: {}", finish.text());
    assert!(env.unlock_kinds(bob.account).is_empty());
    assert_eq!(env.secrets.removes(), [(bob.account, UnlockKind::Prf, Some(key.credential_id().to_vec()))]);
}

#[tokio::test]
async fn passkey_the_eleventh_key_is_a_conflict_on_method() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    for n in 0..10 {
        let (finish, _key) = add_passkey(&env, &bob, &format!("Key {n}"), None).await;
        assert_eq!(finish.status, 200, "key {n}: {}", finish.text());
    }
    let begin = passkey_begin(&env, &bob, "One too many").await;
    assert_code(&begin, 409, json!({"code": "conflict", "field": "method"}), "the eleventh key");
    assert_eq!(passkey_ids(&env, bob.account).len(), 10);
}

#[tokio::test]
async fn passkey_a_credential_for_another_origin_is_invalid_and_stores_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let begin = passkey_begin(&env, &bob, "Evil").await;
    let mut key = SoftPasskey::new();
    let evil = Url::parse("https://evil.example.test").expect("a url");
    let credential = key.register_with(&begin.json()["options"], &evil, &Tweaks::unchecked()).expect("the unchecked path signs");
    let refused = passkey_finish(&env, &bob, &credential, Some(&prf_text(7))).await;
    assert_code(&refused, 401, json!({"code": "invalid"}), "another origin");
    assert_eq!(env.credential_dump(bob.account), before);
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.me.pending_count(), 0, "the challenge is used up");
    let (again, _) = add_passkey(&env, &bob, "Good", None).await;
    assert_eq!(again.status, 200, "the next try works: {}", again.text());
}

#[tokio::test]
async fn passkey_a_damaged_credential_a_missing_begin_and_a_replayed_finish_are_invalid() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let nothing = passkey_finish(&env, &bob, &json!({"id": "x"}), None).await;
    assert_code(&nothing, 401, json!({"code": "invalid"}), "no begin");

    let begin = passkey_begin(&env, &bob, "Key").await;
    let mut key = SoftPasskey::new();
    let mut credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    credential["response"]["clientDataJSON"] = json!("AAAA");
    let damaged = passkey_finish(&env, &bob, &credential, None).await;
    assert_code(&damaged, 401, json!({"code": "invalid"}), "damaged");
    assert_eq!(passkey_ids(&env, bob.account).len(), 0);

    let begin = passkey_begin(&env, &bob, "Key").await;
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    assert_eq!(passkey_finish(&env, &bob, &credential, None).await.status, 200);
    let replay = passkey_finish(&env, &bob, &credential, None).await;
    assert_code(&replay, 401, json!({"code": "invalid"}), "the same response twice");
    assert_eq!(passkey_ids(&env, bob.account).len(), 1);
}

#[tokio::test]
async fn passkey_a_challenge_belongs_to_the_person_who_began_it() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let carol = env.member_with_password("carol").await;
    let begin = passkey_begin(&env, &bob, "Key").await;
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    let stolen = passkey_finish(&env, &carol, &credential, None).await;
    assert_code(&stolen, 401, json!({"code": "invalid"}), "carol has no challenge");
    assert_eq!(passkey_ids(&env, carol.account).len(), 0);
    assert_eq!(passkey_finish(&env, &bob, &credential, None).await.status, 200, "bob's challenge is still bob's");
}

#[tokio::test]
async fn passkey_a_challenge_ends_after_five_minutes() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let begin = passkey_begin(&env, &bob, "Key").await;
    let mut key = SoftPasskey::new();
    let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
    env.clock.advance(301);
    let again = env.another_session("bob");
    assert_code(&passkey_finish(&env, &again, &credential, None).await, 401, json!({"code": "invalid"}), "late");
    assert_eq!(passkey_ids(&env, bob.account).len(), 0);
}

#[tokio::test]
async fn passkey_the_name_is_checked_and_the_method_can_be_off() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    for (what, body) in [
        ("a number", json!({"step": "begin", "name": 5})),
        ("an object", json!({"step": "begin", "name": {"a": 1}})),
        ("65 characters", json!({"step": "begin", "name": "x".repeat(65)})),
        ("a control character", json!({"step": "begin", "name": "a\u{7}b"})),
    ] {
        let response = env.send(env.post_json("/api/me/passkeys", &body, &bob)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "name"}), what);
    }
    let nameless = env.send(env.post_json("/api/me/passkeys", &json!({"step": "begin"}), &bob)).await;
    assert_eq!(nameless.status, 200, "a name is optional: {}", nameless.text());

    let off = ApiEnv::build(Methods { passkey: false, password: true, oidc: false }, Tuning::default());
    let carol = off.member_with_password("carol").await;
    assert_code(&passkey_begin(&off, &carol, "Key").await, 404, json!({"code": "not_found"}), "the method is off");
}

#[tokio::test]
async fn passkey_a_removal_takes_the_unlock_row_and_the_slot_with_it() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let (finish, key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
    let id = finish.json()["id"].as_i64().expect("an id");
    assert_eq!(env.unlock_kinds(bob.account).len(), 1);

    let removed = env.send(env.delete_as(&format!("/api/me/passkeys/{id}"), &bob)).await;
    assert_no_content(&removed, "the removal");
    assert!(passkey_ids(&env, bob.account).is_empty());
    assert!(env.unlock_kinds(bob.account).is_empty(), "the unlock row of the credential is gone");
    assert_eq!(env.secrets.removes(), [(bob.account, UnlockKind::Prf, Some(key.credential_id().to_vec()))]);
    let lines = action_lines(&env, "auth.passkey.remove");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    println!("Passkeys: removal 204, passkey row and prf unlock row gone, seam remove_slot(Prf, credential), 1 audit line");
}

#[tokio::test]
async fn passkey_a_removal_is_done_even_when_the_seam_cannot_remove_the_slot() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let (finish, _key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
    let id = finish.json()["id"].as_i64().expect("an id");
    env.secrets.fail_remove(Some(SecretsError::Unavailable));
    let removed = env.send(env.delete_as(&format!("/api/me/passkeys/{id}"), &bob)).await;
    assert_no_content(&removed, "the removal");
    assert!(passkey_ids(&env, bob.account).is_empty());
}

#[tokio::test]
async fn passkey_the_last_way_to_sign_in_cannot_be_removed() {
    let env = ApiEnv::new();
    let (pk, _key) = env.member_with_passkey("pk");
    let id = passkey_ids(&env, pk.account)[0];
    let refused = env.send(env.delete_as(&format!("/api/me/passkeys/{id}"), &pk)).await;
    assert_code(&refused, 409, json!({"code": "conflict", "field": "method"}), "the only passkey");
    assert_eq!(passkey_ids(&env, pk.account), [id]);
    assert!(env.secrets.removes().is_empty());
    let lines = action_lines(&env, "auth.passkey.remove");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["result"], "fail");
}

#[tokio::test]
async fn passkey_an_id_that_is_not_the_persons_own_is_not_found_whatever_its_shape() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let (_, _key) = add_passkey(&env, &bob, "Mine", None).await;
    let (eve, _eve_key) = env.member_with_passkey("eve");
    let eve_id = passkey_ids(&env, eve.account)[0];
    let big = "9".repeat(25);
    let ids: Vec<String> = vec![
        eve_id.to_string(),
        "999999".to_owned(),
        "0".to_owned(),
        "abc".to_owned(),
        "-1".to_owned(),
        "1e3".to_owned(),
        "0x10".to_owned(),
        "1.0".to_owned(),
        "%2B1".to_owned(),
        "%E0%A5%A9".to_owned(),
        big,
    ];
    for id in &ids {
        let response = env.send(env.delete_as(&format!("/api/me/passkeys/{id}"), &bob)).await;
        assert_code(&response, 404, json!({"code": "not_found"}), id);
    }
    assert_eq!(passkey_ids(&env, eve.account), [eve_id], "eve's passkey is where it was");
    assert_eq!(passkey_ids(&env, bob.account).len(), 1);
    assert_eq!(env.count("auth.passkey.remove"), 0, "nothing was written down");
    assert!(env.secrets.removes().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------------
// PUT /api/me/passphrase and POST /api/me/recovery
// ---------------------------------------------------------------------------------------------------------------------

const NEW_PASSPHRASE: &str = "an entirely new passphrase";

async fn set_passphrase(env: &ApiEnv, who: &Signed, text: &str) -> Resp {
    env.send(env.put_json("/api/me/passphrase", &json!({"passphrase": text}), who)).await
}

async fn replace_recovery(env: &ApiEnv, who: &Signed) -> Resp {
    env.send(env.post_json("/api/me/recovery", &json!({}), who)).await
}

#[tokio::test]
async fn passphrase_a_new_passphrase_gets_its_own_salt_row_and_slot() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let response = set_passphrase(&env, &bob, NEW_PASSPHRASE).await;
    assert_no_content(&response, "the passphrase");

    let rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, UnlockKind::Passphrase);
    assert_eq!(rows[0].salt.len(), 16, "its own 16-byte salt");
    assert_eq!(rows[0].params, params_text(&env.argon.kdf()));
    assert!(rows[0].credential_id.is_none());
    let password = env.store.read(|tx| tx.password_get(bob.account)).expect("read").expect("a password");
    assert_ne!(rows[0].salt, password.salt, "not the salt of the password");

    let sets = env.secrets.sets();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!((sets[0].0, sets[0].1.kind, sets[0].1.credential_id.as_ref()), (bob.account, UnlockKind::Passphrase, None));
    assert!(sets[0].2.is_none());
    let salt: [u8; 16] = rows[0].salt.as_slice().try_into().expect("16 bytes");
    let expected = kek_from_passphrase(NEW_PASSPHRASE, &salt, &env.argon.kdf()).expect("a key");
    assert_eq!(sets[0].1.key, expected.expose().to_vec(), "the slot is wrapped by the key of the typed passphrase");

    let lines = action_lines(&env, "auth.passphrase.set");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert!(!env.audit_text().contains(NEW_PASSPHRASE));
    let me = env.me_of(&bob).await.json();
    assert_eq!(me["secrets"]["passphrase"], true);
    println!("Secrets: passphrase 204, one passphrase unlock row with a 16-byte salt, seam set_slot(Passphrase) with the key of the text, 1 audit line");
}

#[tokio::test]
async fn passphrase_a_second_passphrase_replaces_the_first() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    assert_no_content(&set_passphrase(&env, &bob, NEW_PASSPHRASE).await, "first");
    let first = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_no_content(&set_passphrase(&env, &bob, "another passphrase again").await, "second");
    let second = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_eq!(second.len(), 1, "one row, not two");
    assert_ne!(first[0].salt, second[0].salt);
    assert_eq!(env.secrets.sets().len(), 2);
    assert_eq!(env.count("auth.passphrase.set"), 2);
}

#[tokio::test]
async fn passphrase_the_policy_is_a_400_on_passphrase_and_a_locked_store_is_a_423() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob.the.builder").await;
    let before = env.credential_dump(bob.account);
    for (what, text) in [
        ("too short", "short"),
        ("the username", "Bob.The.Builder"),
        ("one repeated character", "zzzzzzzzzzzzzz"),
        ("too long", &"ab".repeat(129)),
        ("empty", ""),
    ] {
        let derivations = env.argon.derivations();
        let response = set_passphrase(&env, &bob, text).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "passphrase"}), what);
        assert_eq!(env.argon.derivations(), derivations, "{what}: nothing was derived");
    }
    for (what, body) in [
        ("a number", json!({"passphrase": 5})),
        ("missing", json!({})),
        ("null", json!({"passphrase": null})),
    ] {
        let response = env.send(env.put_json("/api/me/passphrase", &body, &bob)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "passphrase"}), what);
    }
    let extra = env.send(env.put_json("/api/me/passphrase", &json!({"passphrase": NEW_PASSPHRASE, "x": 1}), &bob)).await;
    assert_code(&extra, 400, json!({"code": "bad_request"}), "an unknown member");

    env.gate.set_locked(bob.account, true);
    let derivations = env.argon.derivations();
    let locked = set_passphrase(&env, &bob, NEW_PASSPHRASE).await;
    assert_code(&locked, 423, json!({"code": "locked"}), "a locked store");
    assert_eq!(env.argon.derivations(), derivations);
    assert_eq!(env.credential_dump(bob.account), before);
    assert!(env.secrets.calls().is_empty());
}

#[tokio::test]
async fn passphrase_a_seam_that_cannot_do_it_and_a_row_that_cannot_be_written_leave_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    for (error, status, body) in [
        (SecretsError::Unavailable, 503, json!({"code": "busy"})),
        (SecretsError::Locked, 423, json!({"code": "locked"})),
        (SecretsError::Refused, 409, json!({"code": "conflict", "field": "method"})),
    ] {
        env.secrets.fail_set(Some(error));
        assert_code(&set_passphrase(&env, &bob, NEW_PASSPHRASE).await, status, body, &format!("{error:?}"));
        assert_eq!(env.credential_dump(bob.account), before);
    }
    env.secrets.fail_set(None);
    env.break_writes("hw_broken", "INSERT", "unlock_methods", "");
    assert_code(&set_passphrase(&env, &bob, NEW_PASSPHRASE).await, 503, json!({"code": "busy"}), "the row");
    assert_eq!(env.credential_dump(bob.account), before);
    assert_eq!(env.secrets.removes(), [(bob.account, UnlockKind::Passphrase, None)], "the slot that was set is taken back");
    assert_eq!(env.count("auth.passphrase.set"), 0);
}

#[tokio::test]
async fn recovery_a_new_code_is_shown_once_and_the_slot_and_row_are_made_from_it() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let response = replace_recovery(&env, &bob).await;
    assert_eq!(response.status, 200, "{}", response.text());
    assert_eq!(response.header("cache-control").as_deref(), Some("no-store"));
    assert_eq!(keys_of(&response.json()), ["recovery_code"]);
    let code = response.json()["recovery_code"].as_str().expect("a code").to_owned();
    let groups: Vec<&str> = code.split('-').collect();
    assert_eq!(groups.len(), 8, "eight groups: {code}");
    assert!(groups.iter().all(|g| g.len() == 4 && g.bytes().all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))));

    let rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].kind, rows[0].params.as_str()), (UnlockKind::Recovery, HKDF));
    assert_eq!(rows[0].salt.len(), 16);
    let sets = env.secrets.sets();
    assert_eq!(sets.len(), 1);
    assert_eq!((sets[0].0, sets[0].1.kind), (bob.account, UnlockKind::Recovery));
    let parsed = parse_recovery_code(&code).expect("a code");
    let expected = kek_from_recovery(&parsed, &rows[0].salt).expect("a key");
    assert_eq!(sets[0].1.key, expected.expose().to_vec(), "the slot is wrapped by the key of the shown code");

    let lines = action_lines(&env, "recovery.replace");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], bob.account.get().to_string());
    let text = env.audit_text();
    assert!(!text.contains(&code) && !text.replace('-', "").contains(&code.replace('-', "")), "the code is in no audit line");
    assert!(!env.me_of(&bob).await.text().contains(&code), "nor in a later answer");
    println!("Secrets: recovery 200 {{recovery_code}} (8 groups of 4), one recovery unlock row with a 16-byte salt, seam set_slot(Recovery) with the key of the code, 1 audit line recovery.replace");
}

#[tokio::test]
async fn recovery_a_new_code_replaces_the_old_one() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let first = replace_recovery(&env, &bob).await.json()["recovery_code"].as_str().expect("a code").to_owned();
    let first_rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    let second = replace_recovery(&env, &bob).await.json()["recovery_code"].as_str().expect("a code").to_owned();
    let second_rows = env.store.read(|tx| tx.unlock_list(bob.account)).expect("rows");
    assert_ne!(first, second);
    assert_eq!(second_rows.len(), 1, "one recovery row");
    assert_ne!(first_rows[0].salt, second_rows[0].salt, "a new salt");
    assert_eq!(env.count("recovery.replace"), 2);
}

#[tokio::test]
async fn recovery_a_locked_store_a_failing_seam_and_a_failing_row_give_no_code() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);

    env.gate.set_locked(bob.account, true);
    let locked = replace_recovery(&env, &bob).await;
    assert_code(&locked, 423, json!({"code": "locked"}), "locked");
    env.gate.set_locked(bob.account, false);
    assert!(env.secrets.calls().is_empty());

    for (error, status, body) in [
        (SecretsError::Unavailable, 503, json!({"code": "busy"})),
        (SecretsError::Locked, 423, json!({"code": "locked"})),
        (SecretsError::Refused, 409, json!({"code": "conflict", "field": "method"})),
    ] {
        env.secrets.fail_set(Some(error));
        let response = replace_recovery(&env, &bob).await;
        assert_code(&response, status, body, &format!("{error:?}"));
        assert!(!response.text().contains("recovery_code"));
        assert_eq!(env.credential_dump(bob.account), before);
    }
    env.secrets.fail_set(None);

    env.break_writes("hw_broken", "INSERT", "unlock_methods", "");
    let broken = replace_recovery(&env, &bob).await;
    assert_code(&broken, 503, json!({"code": "busy"}), "the row");
    assert_eq!(env.credential_dump(bob.account), before);
    assert_eq!(env.secrets.removes(), [(bob.account, UnlockKind::Recovery, None)]);
    assert_eq!(env.count("recovery.replace"), 0, "no success was written down");
}

#[tokio::test]
async fn recovery_the_body_is_an_empty_object() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    for (what, body) in [("a member", json!({"x": 1})), ("an array", json!([])), ("a string", json!("x"))] {
        let response = env.send(env.post_json("/api/me/recovery", &body, &bob)).await;
        assert_code(&response, 400, json!({"code": "bad_request"}), what);
    }
    assert!(env.secrets.calls().is_empty());
}

// ---------------------------------------------------------------------------------------------------------------------
// DELETE /api/me/sessions/{handle} and DELETE /api/me/sessions
// ---------------------------------------------------------------------------------------------------------------------

fn handle_of(who: &Signed) -> String {
    URL_SAFE_NO_PAD.encode(&who.session.id_hash[..12])
}

fn signout_lines(env: &ApiEnv) -> Vec<Value> {
    lines_of(env, "signout")
}

#[tokio::test]
async fn sessions_another_session_of_the_person_can_be_ended_by_its_handle() {
    let env = ApiEnv::new();
    let here = env.member("bob");
    let there = env.another_session("bob");
    let response = env.send(env.delete_as(&format!("/api/me/sessions/{}", handle_of(&there)), &here)).await;
    assert_no_content(&response, "ending the other session");
    assert!(response.all("set-cookie").is_empty(), "this browser keeps its cookie");
    assert_eq!(env.session_rows(here.account), 1);
    assert_eq!(env.me_of(&there).await.status, 401, "the ended session is gone");
    assert_eq!(env.me_of(&here).await.status, 200);
    let lines = signout_lines(&env);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["actor"], here.account.get().to_string());
    assert_eq!(lines[0]["result"], "ok");
    println!("Sessions: DELETE by handle 204, the other session is gone (401), 1 signout line, no Set-Cookie");
}

#[tokio::test]
async fn sessions_ending_this_session_by_its_handle_deletes_the_cookie() {
    let env = ApiEnv::new();
    let here = env.member("bob");
    let other = env.another_session("bob");
    let response = env.send(env.delete_as(&format!("/api/me/sessions/{}", handle_of(&here)), &here)).await;
    assert_no_content(&response, "ending this session");
    let cookies = response.all("set-cookie");
    assert_eq!(cookies, ["__Host-hw_session=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]);
    assert_eq!(env.me_of(&here).await.status, 401);
    assert_eq!(env.me_of(&other).await.status, 200);
    assert_eq!(signout_lines(&env).len(), 1);
}

#[tokio::test]
async fn sessions_a_handle_of_another_person_or_of_no_session_is_not_found_and_changes_nothing() {
    let env = ApiEnv::new();
    let bob = env.member("bob");
    let eve = env.member("eve");
    let unknown = URL_SAFE_NO_PAD.encode([0xAB; 12]);
    let handles: Vec<String> = vec![
        handle_of(&eve),
        unknown,
        "AAAAAAAAAAAAAAAA".to_owned(),
        "short".to_owned(),
        "A".repeat(17),
        "A".repeat(43),
        bob.cookie.clone(),
        "AAAAAAAAAAAAAAA*".to_owned(),
        "AAAAAAAAAAAAAAA%2F".to_owned(),
        "..".to_owned(),
        "%00".to_owned(),
        "AAAAAAAAAAAAAAA=".to_owned(),
    ];
    for handle in &handles {
        let response = env.send(env.delete_as(&format!("/api/me/sessions/{handle}"), &bob)).await;
        assert_code(&response, 404, json!({"code": "not_found"}), handle);
    }
    assert_eq!(env.session_rows(eve.account), 1, "eve's session is where it was");
    assert_eq!(env.session_rows(bob.account), 1);
    assert_eq!(env.me_of(&eve).await.status, 200);
    assert!(signout_lines(&env).is_empty(), "nothing was written down");
}

#[tokio::test]
async fn sessions_the_cookie_value_is_not_a_handle() {
    let env = ApiEnv::new();
    let here = env.member("bob");
    let there = env.another_session("bob");
    let response = env.send(env.delete_as(&format!("/api/me/sessions/{}", there.cookie), &here)).await;
    assert_code(&response, 404, json!({"code": "not_found"}), "a whole cookie value");
    assert_eq!(env.me_of(&there).await.status, 200, "the session is still alive");
}

#[tokio::test]
async fn sessions_ending_all_ends_this_one_too_and_deletes_the_cookie() {
    let env = ApiEnv::new();
    let here = env.member("bob");
    let there = env.another_session("bob");
    let eve = env.member("eve");
    let response = env.send(env.delete_as("/api/me/sessions", &here)).await;
    assert_no_content(&response, "ending all");
    assert_eq!(response.all("set-cookie"), ["__Host-hw_session=; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"]);
    assert_eq!(env.session_rows(here.account), 0);
    assert_eq!(env.me_of(&here).await.status, 401);
    assert_eq!(env.me_of(&there).await.status, 401);
    assert_eq!(env.me_of(&eve).await.status, 200, "another person's sessions are untouched");
    let lines = signout_lines(&env);
    assert_eq!(lines.len(), 1, "one line for the whole call: {lines:?}");
    assert_eq!(lines[0]["actor"], here.account.get().to_string());
    println!("Sessions: DELETE all 204, every session of the account gone, cookie deleted, 1 signout line, other accounts untouched");
}

#[tokio::test]
async fn sessions_a_call_with_an_old_sign_in_is_refused_and_ends_nothing() {
    let env = ApiEnv::new();
    let bob = env.stale_member_with_password("bob").await;
    let there = env.another_session("bob");
    let one = env.send(env.delete_as(&format!("/api/me/sessions/{}", handle_of(&there)), &bob)).await;
    assert_code(&one, 403, json!({"code": "forbidden", "step_up": true}), "one");
    let all = env.send(env.delete_as("/api/me/sessions", &bob)).await;
    assert_code(&all, 403, json!({"code": "forbidden", "step_up": true}), "all");
    assert_eq!(env.session_rows(bob.account), 2);
}

// ---------------------------------------------------------------------------------------------------------------------
// The gates of every route of the person's own account
// ---------------------------------------------------------------------------------------------------------------------

/// Every change route of `/api/me` with a body that would be valid.
fn change_routes() -> Vec<(Method, &'static str, Value)> {
    vec![
        (Method::POST, "/api/me/password", change_body(PASSWORD, NEW_PASSWORD)),
        (Method::POST, "/api/me/totp", json!({"step": "begin"})),
        (Method::POST, "/api/me/passkeys", json!({"step": "begin", "name": "Key"})),
        (Method::DELETE, "/api/me/passkeys/1", json!({})),
        (Method::PUT, "/api/me/passphrase", json!({"passphrase": NEW_PASSPHRASE})),
        (Method::POST, "/api/me/recovery", json!({})),
        (Method::DELETE, "/api/me/sessions/AAAAAAAAAAAAAAAA", json!({})),
        (Method::DELETE, "/api/me/sessions", json!({})),
    ]
}

#[tokio::test]
async fn gates_every_route_of_the_account_needs_a_session() {
    let env = ApiEnv::new();
    assert_code(&env.send(env.get("/api/me")).await, 401, json!({"code": "invalid"}), "GET /api/me");
    for (method, path, body) in change_routes() {
        let req = edge::Req::new(method.clone(), path)
            .header("host", edge::HOST)
            .header("origin", edge::ORIGIN)
            .header("content-type", "application/json")
            .json(&body);
        let response = env.send(req).await;
        assert_code(&response, 401, json!({"code": "invalid"}), &format!("{method} {path}"));
    }
    assert!(env.audit_lines().is_empty());
}

#[tokio::test]
async fn gates_every_change_needs_a_recent_sign_in_and_changes_nothing_without_one() {
    let env = ApiEnv::new();
    let bob = env.stale_member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    let derivations = env.argon.derivations();
    for (method, path, body) in change_routes() {
        let response = env.send(env.change_as(method.clone(), path, &body, &bob)).await;
        assert_code(&response, 403, json!({"code": "forbidden", "step_up": true}), &format!("{method} {path}"));
    }
    assert_eq!(env.credential_dump(bob.account), before);
    assert_eq!(env.argon.derivations(), derivations, "nothing was derived");
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.me.pending_count(), 0);
    assert!(env.audit_lines().is_empty(), "no audit line at all: {:?}", env.audit_lines());
    // After a step-up the same call goes through.
    let step_up = env.send(env.post_json("/auth/step-up", &json!({"password": PASSWORD}), &bob)).await;
    assert_eq!(step_up.status, 200, "{}", step_up.text());
    let fresh = Signed { session: env.store.read(|tx| tx.session_get(&bob.session.id_hash)).expect("read").expect("row"), ..bob };
    let again = env.send(env.post_json("/api/me/recovery", &json!({}), &fresh)).await;
    assert_eq!(again.status, 200, "{}", again.text());
}

#[tokio::test]
async fn gates_a_change_without_the_token_or_from_another_origin_is_refused_and_changes_nothing() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let before = env.credential_dump(bob.account);
    for (method, path, body) in change_routes() {
        let no_token = edge::Req::new(method.clone(), path)
            .header("host", edge::HOST)
            .header("origin", edge::ORIGIN)
            .header("content-type", "application/json")
            .json(&body)
            .cookie(&bob.cookie);
        assert_code(&env.send(no_token).await, 403, json!({"code": "forbidden"}), &format!("{method} {path} without a token"));
        let foreign = env.change_as(method.clone(), path, &body, &bob).set("origin", "https://evil.example.test");
        assert_code(&env.send(foreign).await, 403, json!({"code": "forbidden"}), &format!("{method} {path} from another origin"));
    }
    assert_eq!(env.credential_dump(bob.account), before);
    assert!(env.audit_lines().is_empty());
    assert!(env.secrets.calls().is_empty());
}

#[tokio::test]
async fn gates_each_person_changes_only_their_own_account() {
    let env = ApiEnv::new();
    let bob = env.member_with_password("bob").await;
    let carol = env.member_with_password("carol").await;
    let carol_before = env.credential_dump(carol.account);
    // Every change of bob's session lands on bob; no body can name another account.
    assert_eq!(set_passphrase(&env, &bob, NEW_PASSPHRASE).await.status, 204);
    assert_eq!(replace_recovery(&env, &bob).await.status, 200);
    let body = json!({"passphrase": NEW_PASSPHRASE, "account": carol.account.get()});
    assert_eq!(env.send(env.put_json("/api/me/passphrase", &body, &bob)).await.status, 400, "no member names another account");
    assert_eq!(env.credential_dump(carol.account), carol_before);
    assert!(env.secrets.sets().iter().all(|(account, _, _)| *account == bob.account));
}

// ---------------------------------------------------------------------------------------------------------------------
// The changes an administrator makes: roles, states, the reset of secrets, the sign-in lock and deletion
// ---------------------------------------------------------------------------------------------------------------------

fn id_of(env: &ApiEnv, name: &str) -> AccountId {
    env.store.account_by_username(name).expect("read").expect("the account exists").id
}

/// Everything about an account that a change could touch, as a text, to see that a refused call left it alone.
fn account_text(env: &ApiEnv, id: AccountId) -> String {
    format!(
        "{:?}|{}|{}",
        env.store.read(|tx| tx.account_get(id)).expect("read"),
        env.credential_dump(id),
        env.session_rows(id)
    )
}

fn role_now(env: &ApiEnv, id: AccountId) -> Role {
    env.store.read(|tx| tx.account_get(id)).expect("read").expect("an account").role
}

fn state_now(env: &ApiEnv, id: AccountId) -> AccountState {
    env.store.read(|tx| tx.account_get(id)).expect("read").expect("an account").state
}

/// An administrator with a password whose sign-in is `age` seconds old.
async fn admin_with_password(env: &ApiEnv, name: &str, age: i64) -> Signed {
    let id = env.person(name).await;
    env.store.write(|tx| tx.account_set_role(id, Role::Admin)).expect("promote");
    env.signed_aged(name, age)
}

/// The six changes of one account with a body that would be valid: name, method, path, body.
fn admin_changes(id: AccountId) -> Vec<(&'static str, Method, String, Value)> {
    let base = format!("/api/admin/accounts/{}", id.get());
    vec![
        ("role", Method::POST, format!("{base}/role"), json!({"role": "admin"})),
        ("disable", Method::POST, format!("{base}/disable"), json!({})),
        ("enable", Method::POST, format!("{base}/enable"), json!({})),
        ("reset-secrets", Method::POST, format!("{base}/reset-secrets"), json!({})),
        ("unlock-signin", Method::POST, format!("{base}/unlock-signin"), json!({})),
        ("delete", Method::DELETE, base, json!({})),
    ]
}

fn request_of(env: &ApiEnv, method: &Method, path: &str, body: &Value, who: &Signed) -> edge::Req {
    if *method == Method::DELETE { env.delete_as(path, who) } else { env.change_as(method.clone(), path, body, who) }
}

async fn change_role(env: &ApiEnv, who: &Signed, target: AccountId, role: &str) -> Resp {
    let path = format!("/api/admin/accounts/{}/role", target.get());
    env.send(env.post_json(&path, &json!({"role": role}), who)).await
}

async fn admin_post(env: &ApiEnv, who: &Signed, target: AccountId, tail: &str) -> Resp {
    let path = format!("/api/admin/accounts/{}/{tail}", target.get());
    env.send(env.post_json(&path, &json!({}), who)).await
}

async fn delete_account(env: &ApiEnv, who: &Signed, target: AccountId) -> Resp {
    env.send(env.delete_as(&format!("/api/admin/accounts/{}", target.get()), who)).await
}

#[tokio::test]
async fn roles_a_member_becomes_an_administrator_and_the_audit_line_says_which_role() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let response = change_role(&env, &alice, bob, "admin").await;
    assert_no_content(&response, "the promotion");
    assert_eq!(role_now(&env, bob), Role::Admin);
    let lines = lines_of(&env, "account.role");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], alice.account.get().to_string());
    assert_eq!(lines[0]["target"], bob.get().to_string());
    assert_eq!(lines[0]["detail"]["role"], "admin");
    assert_eq!(env.send(env.get_as("/api/admin/accounts", &env.signed_aged("bob", 0))).await.status, 200, "bob is one now");

    // The same role again is not an error and is written down once more.
    assert_no_content(&change_role(&env, &alice, bob, "admin").await, "the same role");
    assert_eq!(env.count("account.role"), 2);
    println!("Roles: member -> admin 204, account.role ok actor=alice target=bob detail role=admin, 1 line per call");
}

#[tokio::test]
async fn roles_an_administrator_becomes_a_member_when_another_one_is_left() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.admin_fresh("bob");
    let bob = id_of(&env, "bob");
    assert_no_content(&change_role(&env, &alice, bob, "member").await, "the demotion");
    assert_eq!(role_now(&env, bob), Role::Member);
    // Bob's session is alive, but it is a member's session now.
    let refused = env.send(env.get_as("/api/admin/accounts", &env.signed_aged("bob", 0))).await;
    assert_code(&refused, 403, json!({"code": "forbidden"}), "a demoted administrator");
    let own = change_role(&env, &alice, alice.account, "member").await;
    assert_code(&own, 409, json!({"code": "conflict", "field": "role"}), "the last administrator demotes themselves");
    assert_eq!(role_now(&env, alice.account), Role::Admin);
    let lines = lines_of(&env, "account.role");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["result"], "fail", "the refusal is written down once: {lines:?}");
}

#[tokio::test]
async fn roles_a_role_the_body_does_not_name_is_a_400_on_role_and_changes_nothing() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let before = account_text(&env, bob);
    let path = format!("/api/admin/accounts/{}/role", bob.get());
    for (what, body) in [
        ("missing", json!({})),
        ("null", json!({"role": null})),
        ("a number", json!({"role": 1})),
        ("a list", json!({"role": ["admin"]})),
        ("an unknown role", json!({"role": "root"})),
        ("another case", json!({"role": "Admin"})),
        ("spaces", json!({"role": " admin"})),
        ("empty", json!({"role": ""})),
    ] {
        let response = env.send(env.post_json(&path, &body, &alice)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "role"}), what);
    }
    let extra = env.send(env.post_json(&path, &json!({"role": "admin", "actor": 1}), &alice)).await;
    assert_code(&extra, 400, json!({"code": "bad_request"}), "an unknown member");
    assert_eq!(account_text(&env, bob), before);
    assert_eq!(env.count("account.role"), 0, "a body that never reached the service writes no line");
}

#[tokio::test]
async fn states_a_disabled_account_has_no_session_cannot_sign_in_and_the_worker_is_told() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    let second = env.another_session("bob");
    let uid = env.store.read(|tx| tx.account_get(bob.account)).expect("read").expect("account").uid;

    let response = admin_post(&env, &alice, bob.account, "disable").await;
    assert_no_content(&response, "the disabling");
    assert_eq!(state_now(&env, bob.account), AccountState::Disabled);
    assert_eq!(env.session_rows(bob.account), 0, "every session is gone");
    assert_eq!(env.me_of(&bob).await.status, 401);
    assert_eq!(env.me_of(&second).await.status, 401);
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 401, "the password path says what it says to a wrong password");
    assert_eq!(env.observer.ended(), [(bob.account, uid, Ending::Disabled)]);
    let lines = lines_of(&env, "account.disable");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["detail"]["state"], "disabled");
    let list = env.send(env.get_as("/api/admin/accounts", &alice)).await.json();
    assert_eq!(by_name(&list, "bob")["state"], "disabled");
    println!("Roles: disable 204, state disabled, every session gone, worker told (Disabled), 1 account.disable line");
}

#[tokio::test]
async fn states_enabling_an_account_lets_the_person_sign_in_again() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    assert_no_content(&admin_post(&env, &alice, bob.account, "disable").await, "disable");
    assert_no_content(&admin_post(&env, &alice, bob.account, "enable").await, "enable");
    assert_eq!(state_now(&env, bob.account), AccountState::Active);
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 200);
    let lines = lines_of(&env, "account.enable");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["detail"]["state"], "active");
    assert_eq!(env.observer.ended().len(), 1, "enabling tells no one that anything ended");
}

#[tokio::test]
async fn states_the_last_administrator_can_be_neither_disabled_nor_deleted() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let before = account_text(&env, alice.account);
    let disable = admin_post(&env, &alice, alice.account, "disable").await;
    assert_code(&disable, 409, json!({"code": "conflict", "field": "role"}), "disable");
    let delete = delete_account(&env, &alice, alice.account).await;
    assert_code(&delete, 409, json!({"code": "conflict", "field": "role"}), "delete");
    assert_eq!(account_text(&env, alice.account), before);
    assert!(env.observer.ended().is_empty());
    assert_eq!(env.me_of(&alice).await.status, 200);
    // With another administrator the same calls work, on either of them.
    env.admin_fresh("bob");
    assert_no_content(&delete_account(&env, &alice, id_of(&env, "bob")).await, "bob goes");
}

#[tokio::test]
async fn states_a_deleted_account_loses_its_credentials_and_sessions_and_keeps_its_name() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    let second = env.another_session("bob");
    let uid = env.store.read(|tx| tx.account_get(bob.account)).expect("read").expect("account").uid;
    env.store.write(|tx| tx.unlock_add(bob.account, UnlockKind::Recovery, &[1; 16], HKDF, None)).expect("a row");

    assert_no_content(&delete_account(&env, &alice, bob.account).await, "the deletion");
    assert_eq!(state_now(&env, bob.account), AccountState::Deleted);
    assert_eq!(env.session_rows(bob.account), 0);
    assert_eq!(env.me_of(&bob).await.status, 401);
    assert_eq!(env.me_of(&second).await.status, 401);
    assert!(env.unlock_kinds(bob.account).is_empty());
    assert_eq!(env.credential_dump(bob.account).trim(), "", "no credential is left");
    assert_eq!(env.observer.ended(), [(bob.account, uid, Ending::Deleted)]);
    let lines = lines_of(&env, "account.delete");
    assert_eq!(lines.len(), 1, "{lines:?}");
    let list = env.send(env.get_as("/api/admin/accounts", &alice)).await.json();
    assert!(list["accounts"].as_array().expect("a list").iter().all(|a| a["username"] != "bob"));
    // The name is not given to anybody else.
    let again = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "username": "bob"}), &alice)).await;
    assert_code(&again, 409, json!({"code": "conflict", "field": "username"}), "the name stays reserved");
    // A deleted account is no target for anything.
    for (name, method, path, body) in admin_changes(bob.account) {
        let response = env.send(request_of(&env, &method, &path, &body, &alice)).await;
        assert_code(&response, 404, json!({"code": "not_found"}), name);
    }
}

#[tokio::test]
async fn reset_secrets_removes_the_three_unlock_kinds_and_keeps_the_password() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    env.store
        .write(|tx| {
            tx.unlock_add(bob.account, UnlockKind::Password, &[1; 16], &params_text(&env.argon.kdf()), None)?;
            tx.unlock_add(bob.account, UnlockKind::Passphrase, &[2; 16], &params_text(&env.argon.kdf()), None)?;
            tx.unlock_add(bob.account, UnlockKind::Prf, &[3; 32], HKDF, Some(&[4; 16]))?;
            tx.unlock_add(bob.account, UnlockKind::Recovery, &[5; 16], HKDF, None)?;
            Ok(())
        })
        .expect("rows");
    let other = env.member_with_password("carol").await;
    env.store.write(|tx| tx.unlock_add(other.account, UnlockKind::Recovery, &[6; 16], HKDF, None)).expect("a row");
    let others = account_text(&env, other.account);

    let response = admin_post(&env, &alice, bob.account, "reset-secrets").await;
    assert_no_content(&response, "the reset");
    assert_eq!(env.secrets.resets(), [bob.account], "the seam was asked once, for that account");
    let kinds: Vec<UnlockKind> = env.unlock_kinds(bob.account).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [UnlockKind::Password], "only the way in by password is left");
    assert_eq!(account_text(&env, other.account), others, "another person's rows are not touched");
    assert_eq!(password_status(&env, "bob", PASSWORD).await, 200, "the password itself works");
    assert_eq!(env.session_rows(bob.account), 1, "no session is ended");
    let lines = lines_of(&env, "secrets.reset");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], alice.account.get().to_string());
    assert_eq!(lines[0]["target"], bob.account.get().to_string());
    println!("Reset: reset-secrets 204, seam reset(bob) once, passphrase/prf/recovery rows gone, password row kept, 1 secrets.reset line");
}

#[tokio::test]
async fn reset_secrets_leaves_the_rows_and_the_audit_alone_when_the_seam_cannot() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    env.store.write(|tx| tx.unlock_add(bob.account, UnlockKind::Recovery, &[5; 16], HKDF, None)).expect("a row");
    let before = account_text(&env, bob.account);
    for (error, status, body) in [
        (SecretsError::Unavailable, 503, json!({"code": "busy"})),
        (SecretsError::Locked, 423, json!({"code": "locked"})),
        (SecretsError::Refused, 409, json!({"code": "conflict", "field": "method"})),
    ] {
        env.secrets.fail_reset(Some(error));
        let response = admin_post(&env, &alice, bob.account, "reset-secrets").await;
        assert_code(&response, status, body, &format!("{error:?}"));
        assert_eq!(account_text(&env, bob.account), before);
    }
    assert_eq!(env.count("secrets.reset"), 0, "a reset that did not happen is not written down as one");

    env.secrets.fail_reset(None);
    env.break_writes("hw_broken", "DELETE", "unlock_methods", "");
    let broken = admin_post(&env, &alice, bob.account, "reset-secrets").await;
    assert_code(&broken, 503, json!({"code": "busy"}), "the rows");
    assert_eq!(env.unlock_kinds(bob.account).len(), 2, "the rows are as they were");
    assert_eq!(env.count("secrets.reset"), 0);
    env.repair("hw_broken");
    assert_no_content(&admin_post(&env, &alice, bob.account, "reset-secrets").await, "the retry");
    assert_eq!(env.unlock_kinds(bob.account).len(), 1);
    assert_eq!(env.count("secrets.reset"), 1, "the retry is the one that is written down");
}

#[tokio::test]
async fn reset_secrets_of_an_account_that_is_not_there_asks_the_seam_nothing() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let response = env.send(env.post_json("/api/admin/accounts/987654/reset-secrets", &json!({}), &alice)).await;
    assert_code(&response, 404, json!({"code": "not_found"}), "no such account");
    assert!(env.secrets.calls().is_empty());
    assert_eq!(env.count("secrets.reset"), 0);
}

#[tokio::test]
async fn lock_an_administrator_lifts_the_lock_of_the_sign_in() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let dave = env.person("dave").await;
    for _ in 0..5 {
        let refused = env.send(env.post("/auth/password", &json!({"username": "dave", "password": WRONG}))).await;
        assert!(matches!(refused.status.as_u16(), 401 | 429));
    }
    assert_eq!(password_status(&env, "dave", PASSWORD).await, 429, "dave is locked out");
    let list = env.send(env.get_as("/api/admin/accounts", &alice)).await.json();
    assert_eq!(by_name(&list, "dave")["state"], "locked_out");

    let response = admin_post(&env, &alice, dave, "unlock-signin").await;
    assert_no_content(&response, "the unlock");
    assert_eq!(password_status(&env, "dave", PASSWORD).await, 200, "the next try is not throttled on the name");
    let list = env.send(env.get_as("/api/admin/accounts", &alice)).await.json();
    assert_eq!(by_name(&list, "dave")["state"], "active");
    let lines = lines_of(&env, "account.unlock");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["actor"], alice.account.get().to_string());
    assert_eq!(lines[0]["target"], dave.get().to_string());
    println!("Lock: 5 wrong passwords lock dave (429, listed locked_out), unlock-signin 204, the next sign-in 200, listed active, 1 account.unlock line");
}

#[tokio::test]
async fn lock_unlocking_a_person_who_is_not_locked_is_fine_and_an_unknown_one_is_not_found() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member("bob");
    assert_no_content(&admin_post(&env, &alice, bob.account, "unlock-signin").await, "not locked");
    assert_eq!(env.count("account.unlock"), 1);
    let unknown = env.send(env.post_json("/api/admin/accounts/987654/unlock-signin", &json!({}), &alice)).await;
    assert_code(&unknown, 404, json!({"code": "not_found"}), "unknown");
    let lines = lines_of(&env, "account.unlock");
    assert_eq!(lines.len(), 2, "one line per call: {lines:?}");
    assert_eq!(lines[1]["result"], "fail");
}

// ---------------------------------------------------------------------------------------------------------------------
// Targets, the actor and the three gates
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn targets_an_id_that_is_not_a_plain_number_or_not_an_account_is_not_found_on_every_route() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let before = account_text(&env, bob);
    let lines = env.audit_lines().len();
    let ids: Vec<String> = vec![
        "0".to_owned(),
        "987654".to_owned(),
        "abc".to_owned(),
        "-1".to_owned(),
        "+1".to_owned(),
        "1.0".to_owned(),
        "1e3".to_owned(),
        "0x1".to_owned(),
        "%20".to_owned(),
        "1%20".to_owned(),
        "%E0%A5%A9".to_owned(),
        "..".to_owned(),
        "9".repeat(20),
        "9".repeat(21),
        "9".repeat(5_000),
        format!("{}%00", bob.get()),
        format!("{}/", bob.get()),
    ];
    let routes = ["role", "disable", "enable", "reset-secrets", "unlock-signin"];
    for id in &ids {
        for tail in routes {
            let body = if tail == "role" { json!({"role": "admin"}) } else { json!({}) };
            let response = env.send(env.post_json(&format!("/api/admin/accounts/{id}/{tail}"), &body, &alice)).await;
            assert!(
                response.status == 404 && (response.json() == json!({"code": "not_found"}) || response.body.is_empty()),
                "{tail} with id {:?}: {} {}",
                &id[..id.len().min(40)],
                response.status,
                response.text()
            );
        }
        let response = env.send(env.delete_as(&format!("/api/admin/accounts/{id}"), &alice)).await;
        assert_eq!(response.status, 404, "delete with id {:?}", &id[..id.len().min(40)]);
    }
    assert_eq!(account_text(&env, bob), before, "bob is as he was");
    assert_eq!(state_now(&env, alice.account), AccountState::Active);
    assert_eq!(env.audit_lines().len(), lines, "nothing was written down");
    assert!(env.secrets.calls().is_empty());
    assert!(env.observer.ended().is_empty());
}

#[tokio::test]
async fn targets_the_actor_is_always_the_session_and_no_body_names_one() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let carol = env.admin_fresh("carol");
    env.member("bob");
    let bob = id_of(&env, "bob");
    for (who, name) in [(&alice, "alice"), (&carol, "carol")] {
        for member in ["actor", "actor_id", "account", "by", "admin", "user"] {
            let path = format!("/api/admin/accounts/{}/role", bob.get());
            let body = json!({"role": "member", member: who.account.get()});
            let response = env.send(env.post_json(&path, &body, who)).await;
            assert_code(&response, 400, json!({"code": "bad_request"}), &format!("{name} names {member}"));
        }
    }
    assert_no_content(&change_role(&env, &alice, bob, "admin").await, "alice");
    assert_no_content(&change_role(&env, &carol, bob, "member").await, "carol");
    let lines = lines_of(&env, "account.role");
    let actors: Vec<&str> = lines.iter().map(|l| l["actor"].as_str().expect("actor")).collect();
    assert_eq!(actors, [alice.account.get().to_string(), carol.account.get().to_string()]);
}

#[tokio::test]
async fn gates_every_change_of_the_administration_needs_a_session() {
    let env = ApiEnv::new();
    env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let mut routes = admin_changes(bob);
    routes.push(("invitation", Method::POST, "/api/admin/invitations".to_owned(), json!({"role": "member"})));
    for (name, method, path, body) in routes {
        let req = edge::Req::new(method, &path)
            .header("host", edge::HOST)
            .header("origin", edge::ORIGIN)
            .header("content-type", "application/json")
            .json(&body);
        assert_code(&env.send(req).await, 401, json!({"code": "invalid"}), name);
    }
    assert_eq!(state_now(&env, bob), AccountState::Active);
    assert_eq!(env.invitation_rows(), 0);
}

#[tokio::test]
async fn gates_a_member_is_forbidden_without_a_step_up_hint_before_the_body_is_read() {
    let env = ApiEnv::new();
    env.admin_fresh("alice");
    let bob = env.member("bob");
    let carol = env.member("carol");
    let before = account_text(&env, carol.account);
    let mut routes = admin_changes(carol.account);
    routes.push(("invitation", Method::POST, "/api/admin/invitations".to_owned(), json!({"role": "member"})));
    for (name, method, path, body) in &routes {
        let response = env.send(request_of(&env, method, path, body, &bob)).await;
        assert_code(&response, 403, json!({"code": "forbidden"}), name);
        // A body that cannot be read is not read: the answer is the same.
        let broken = request_of(&env, method, path, body, &bob).bytes(b"{ not json".to_vec());
        assert_code(&env.send(broken).await, 403, json!({"code": "forbidden"}), &format!("{name} with a broken body"));
    }
    assert_eq!(account_text(&env, carol.account), before);
    assert_eq!(env.invitation_rows(), 0);
    assert!(env.secrets.calls().is_empty());
    assert!(env.audit_lines().is_empty(), "a refusal at the gate writes nothing: {:?}", env.audit_lines());
    println!("Gates: 7 administration changes: no session 401 invalid, member 403 forbidden (no step_up, body unread), old sign-in 403 step_up, no token or foreign origin 403, nothing written");
}

#[tokio::test]
async fn gates_an_old_sign_in_needs_a_step_up_first_and_then_the_same_call_works() {
    let env = ApiEnv::new();
    let alice = admin_with_password(&env, "alice", SIX_MINUTES).await;
    env.member("bob");
    let bob = id_of(&env, "bob");
    let before = account_text(&env, bob);
    let mut routes = admin_changes(bob);
    routes.push(("invitation", Method::POST, "/api/admin/invitations".to_owned(), json!({"role": "member"})));
    for (name, method, path, body) in &routes {
        let response = env.send(request_of(&env, method, path, body, &alice)).await;
        assert_code(&response, 403, json!({"code": "forbidden", "step_up": true}), name);
    }
    assert_eq!(account_text(&env, bob), before);
    assert_eq!(env.invitation_rows(), 0);
    assert!(env.secrets.calls().is_empty() && env.observer.ended().is_empty());
    assert_eq!(env.send(env.get_as("/api/admin/accounts", &alice)).await.status, 200, "a read needs no recent sign-in");

    let step_up = env.send(env.post_json("/auth/step-up", &json!({"password": PASSWORD}), &alice)).await;
    assert_eq!(step_up.status, 200, "{}", step_up.text());
    assert_no_content(&admin_post(&env, &alice, bob, "unlock-signin").await, "after the step-up");
    println!("Step-up: 7 changes by an old sign-in 403 {{forbidden, step_up:true}} and nothing written; after /auth/step-up the same kind of call 204");
}

#[tokio::test]
async fn gates_a_change_without_the_token_or_from_another_origin_changes_nothing() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let before = account_text(&env, bob);
    let mut routes = admin_changes(bob);
    routes.push(("invitation", Method::POST, "/api/admin/invitations".to_owned(), json!({"role": "member"})));
    for (name, method, path, body) in &routes {
        let no_token = edge::Req::new(method.clone(), path)
            .header("host", edge::HOST)
            .header("origin", edge::ORIGIN)
            .header("content-type", "application/json")
            .json(body)
            .cookie(&alice.cookie);
        assert_code(&env.send(no_token).await, 403, json!({"code": "forbidden"}), &format!("{name} without a token"));
        let foreign = request_of(&env, method, path, body, &alice).set("origin", "https://evil.example.test");
        assert_code(&env.send(foreign).await, 403, json!({"code": "forbidden"}), &format!("{name} from another origin"));
    }
    assert_eq!(account_text(&env, bob), before);
    assert_eq!(env.invitation_rows(), 0);
    assert!(env.audit_lines().is_empty());
}

#[tokio::test]
async fn gates_an_administrator_who_was_demoted_a_moment_ago_changes_nothing() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob_session = env.admin_fresh("bob");
    env.member("carol");
    let carol = id_of(&env, "carol");
    assert_no_content(&change_role(&env, &alice, bob_session.account, "member").await, "the demotion");
    let before = account_text(&env, carol);
    let mut routes = admin_changes(carol);
    routes.push(("invitation", Method::POST, "/api/admin/invitations".to_owned(), json!({"role": "member"})));
    for (name, method, path, body) in &routes {
        let response = env.send(request_of(&env, method, path, body, &bob_session)).await;
        assert_code(&response, 403, json!({"code": "forbidden"}), name);
    }
    assert_eq!(account_text(&env, carol), before);
    println!("Roles: a demoted administrator's live session is refused 403 forbidden on all 7 changes");
}

// ---------------------------------------------------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn invitations_at_most_two_hundred_are_open_and_a_redeemed_or_expired_one_makes_room() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    for n in 0..200 {
        let response = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "days": 1}), &alice)).await;
        assert_eq!(response.status, 200, "invitation {n}: {}", response.text());
    }
    assert_eq!(env.invitation_rows(), 200);
    let over = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &alice)).await;
    assert_code(&over, 409, json!({"code": "conflict", "field": "invitations"}), "the 201st");
    assert_eq!(env.invitation_rows(), 200);
    let named = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "username": "bob"}), &alice)).await;
    assert_code(&named, 409, json!({"code": "conflict", "field": "invitations"}), "a named account counts the same");
    assert!(env.store.account_by_username("bob").expect("read").is_none(), "and makes no account");
    let failed = lines_of(&env, "account.invite").into_iter().filter(|l| l["result"] == "fail").count();
    assert_eq!(failed, 2, "each refusal is one line");

    env.clock.advance(2 * 86_400);
    let alice = env.signed_aged("alice", 0);
    let again = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &alice)).await;
    assert_eq!(again.status, 200, "the old ones ran out: {}", again.text());
    println!("Invitations: 200 open invitations accepted, the 201st and a named account are 409 conflict field invitations, room again once they expire");
}

#[tokio::test]
async fn invitations_the_days_are_1_to_30_and_default_to_7() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    for (days, expected) in [(json!(1), 1), (json!(30), 30), (json!(14), 14)] {
        let response = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "days": days}), &alice)).await;
        assert_eq!(response.status, 200, "{days}: {}", response.text());
        assert_eq!(response.json()["expires_at"], text_time(env.clock.unix() + expected * 86_400));
    }
    let rows = env.invitation_rows();
    for (what, days) in [
        ("zero", json!(0)),
        ("31", json!(31)),
        ("negative", json!(-1)),
        ("a fraction", json!(1.5)),
        ("text", json!("7")),
        ("null", json!(null)),
        ("a list", json!([7])),
        ("huge", json!(u64::MAX)),
    ] {
        let response = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "days": days}), &alice)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": "days"}), what);
    }
    assert_eq!(env.invitation_rows(), rows);
}

#[tokio::test]
async fn invitations_a_role_a_name_and_a_shown_name_are_checked_and_name_the_member_that_is_wrong() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    for (what, body, field) in [
        ("no role", json!({}), "role"),
        ("a bad role", json!({"role": "owner"}), "role"),
        ("a number as role", json!({"role": 1}), "role"),
        ("a number as name", json!({"role": "member", "username": 5}), "username"),
        ("a name with a space", json!({"role": "member", "username": "bo b"}), "username"),
        ("an upper case name", json!({"role": "member", "username": "Bob"}), "username"),
        ("a name that is too long", json!({"role": "member", "username": "b".repeat(65)}), "username"),
        ("a shown name without a name", json!({"role": "member", "display_name": "Bob"}), "display_name"),
        ("a number as shown name", json!({"role": "member", "username": "bob", "display_name": 5}), "display_name"),
        ("an empty shown name", json!({"role": "member", "username": "bob", "display_name": ""}), "display_name"),
        ("a shown name with a control character", json!({"role": "member", "username": "bob", "display_name": "a\u{7}b"}), "display_name"),
    ] {
        let response = env.send(env.post_json("/api/admin/invitations", &json!(body), &alice)).await;
        assert_code(&response, 400, json!({"code": "bad_request", "field": field}), what);
    }
    let extra = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "x": 1}), &alice)).await;
    assert_code(&extra, 400, json!({"code": "bad_request"}), "an unknown member");
    assert_eq!(env.invitation_rows(), 0);
    assert_eq!(env.count("account.invite"), 0);
}

#[tokio::test]
async fn invitations_a_name_makes_an_account_without_a_sign_in_method_and_the_link_enrols_it() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let response = env
        .send(env.post_json(
            "/api/admin/invitations",
            &json!({"role": "admin", "username": "bob", "display_name": "Bob B", "days": 3}),
            &alice,
        ))
        .await;
    assert_eq!(response.status, 200, "{}", response.text());
    let bob = env.store.account_by_username("bob").expect("read").expect("bob exists");
    assert_eq!((bob.role, bob.display_name.as_str()), (Role::Admin, "Bob B"));
    assert_eq!(response.json()["expires_at"], text_time(env.clock.unix() + 3 * 86_400));
    assert!(env.unlock_kinds(bob.id).is_empty());
    let link = response.json()["link"].as_str().expect("a link").to_owned();
    let token = link.strip_prefix(&format!("{PUBLIC_URL}/#invite=")).expect("the shape").to_owned();
    let (finish, _secret) = env.redeem_with_password(&token, "bob").await;
    assert_eq!(finish.status, 200, "{}", finish.text());

    // The name is taken now: the same name again is a conflict on username, with no second invitation.
    let rows = env.invitation_rows();
    let twice = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "username": "bob"}), &alice)).await;
    assert_code(&twice, 409, json!({"code": "conflict", "field": "username"}), "a name that exists");
    assert_eq!(env.invitation_rows(), rows);
}

// ---------------------------------------------------------------------------------------------------------------------
// Exactly one audit line per change
// ---------------------------------------------------------------------------------------------------------------------

/// One change route with the setup it needs, run in an environment of its own; answers the audit lines the call wrote.
async fn one_change(case: usize) -> (&'static str, Vec<Value>, String) {
    let env = ApiEnv::new();
    let admin = env.admin_fresh("alice");
    let actor = admin.account.get().to_string();
    let target = env.member_with_password("bob").await;
    let mut who: &Signed = &admin;
    let (name, response, before): (&'static str, Resp, usize);
    match case {
        0 => {
            before = env.audit_lines().len();
            name = "account.invite";
            response = env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &admin)).await;
        }
        1 => {
            before = env.audit_lines().len();
            name = "account.role";
            response = change_role(&env, &admin, target.account, "admin").await;
        }
        2 => {
            before = env.audit_lines().len();
            name = "account.disable";
            response = admin_post(&env, &admin, target.account, "disable").await;
        }
        3 => {
            env.set_state("bob", AccountState::Disabled);
            before = env.audit_lines().len();
            name = "account.enable";
            response = admin_post(&env, &admin, target.account, "enable").await;
        }
        4 => {
            before = env.audit_lines().len();
            name = "secrets.reset";
            response = admin_post(&env, &admin, target.account, "reset-secrets").await;
        }
        5 => {
            before = env.audit_lines().len();
            name = "account.unlock";
            response = admin_post(&env, &admin, target.account, "unlock-signin").await;
        }
        6 => {
            before = env.audit_lines().len();
            name = "account.delete";
            response = delete_account(&env, &admin, target.account).await;
        }
        7 => {
            who = &target;
            before = env.audit_lines().len();
            name = "auth.password.change";
            response = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &target)).await;
        }
        8 => {
            who = &target;
            let begin = totp_begin(&env, &target).await;
            let secret = secret_of(&begin);
            let code = totp::code_at(&secret, env.clock.unix()).expect("a code").to_string();
            before = env.audit_lines().len();
            name = "auth.totp.enrol";
            response = totp_finish(&env, &target, &code).await;
        }
        9 => {
            who = &target;
            let begin = passkey_begin(&env, &target, "Key").await;
            let mut key = SoftPasskey::new();
            let credential = key.register(&begin.json()["options"], &env.origin).expect("answers");
            before = env.audit_lines().len();
            name = "auth.passkey.add";
            response = passkey_finish(&env, &target, &credential, None).await;
        }
        10 => {
            who = &target;
            let (added, _key) = add_passkey(&env, &target, "Key", None).await;
            let id = added.json()["id"].as_i64().expect("an id");
            before = env.audit_lines().len();
            name = "auth.passkey.remove";
            response = env.send(env.delete_as(&format!("/api/me/passkeys/{id}"), &target)).await;
        }
        11 => {
            who = &target;
            before = env.audit_lines().len();
            name = "auth.passphrase.set";
            response = set_passphrase(&env, &target, NEW_PASSPHRASE).await;
        }
        12 => {
            who = &target;
            before = env.audit_lines().len();
            name = "recovery.replace";
            response = replace_recovery(&env, &target).await;
        }
        13 => {
            who = &target;
            let other = env.another_session("bob");
            before = env.audit_lines().len();
            name = "signout";
            response = env.send(env.delete_as(&format!("/api/me/sessions/{}", handle_of(&other)), &target)).await;
        }
        _ => {
            who = &target;
            env.another_session("bob");
            before = env.audit_lines().len();
            name = "signout";
            response = env.send(env.delete_as("/api/me/sessions", &target)).await;
        }
    }
    assert!(matches!(response.status.as_u16(), 200 | 204), "{name}: {} {}", response.status, response.text());
    let lines = env.audit_lines().split_off(before);
    let expected_actor = if who.account == admin.account { actor } else { target.account.get().to_string() };
    (name, lines, expected_actor)
}

#[tokio::test]
async fn audit_once_each_of_fifteen_changes_writes_exactly_one_line_with_the_actor_of_the_session() {
    let mut seen = Vec::new();
    for case in 0..15 {
        let (name, lines, actor) = one_change(case).await;
        assert_eq!(lines.len(), 1, "{name}: {lines:?}");
        assert_eq!(lines[0]["action"], name);
        assert_eq!(lines[0]["result"], "ok", "{name}");
        assert_eq!(lines[0]["actor"], actor, "{name}");
        seen.push(name);
    }
    println!("Audit once: 15 change routes, one ok line each with the session's account as actor: {}", seen.join(", "));
}

#[tokio::test]
async fn audit_once_a_refused_change_writes_one_failure_line_and_a_gate_writes_none() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let before = env.audit_lines().len();
    assert_eq!(admin_post(&env, &alice, alice.account, "disable").await.status, 409);
    let lines = env.audit_lines().split_off(before);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!((lines[0]["action"].as_str(), lines[0]["result"].as_str()), (Some("account.disable"), Some("fail")));
}

// ---------------------------------------------------------------------------------------------------------------------
// The race, hostile input, overload, disclosure and the canary
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_two_administrators_demoting_each_other_never_leave_the_service_without_one() {
    let env = Arc::new(ApiEnv::new());
    let alice = Arc::new(env.admin_fresh("alice"));
    let bob = Arc::new(env.admin_fresh("bob"));
    let (mut alice_won, mut bob_won) = (0, 0);
    for round in 0..16 {
        let (a, b) = (alice.clone(), bob.clone());
        let (ea, eb) = (env.clone(), env.clone());
        let first = tokio::spawn(async move { change_role(&ea, &a, b.account, "member").await });
        let (a, b) = (alice.clone(), bob.clone());
        let second = tokio::spawn(async move { change_role(&eb, &b, a.account, "member").await });
        let (first, second) = (first.await.expect("task"), second.await.expect("task"));
        let admins = env.count_rows("SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND state = 'active'");
        assert_eq!(admins, 1, "round {round}: exactly one administrator is left");
        let statuses = (first.status.as_u16(), second.status.as_u16());
        assert!(matches!(statuses, (204, 403) | (403, 204)), "round {round}: one wins and the other is no administrator any more: {statuses:?}");
        // The winner puts the loser back for the next round.
        let (winner, loser) = if statuses.0 == 204 { alice_won += 1; (&alice, &bob) } else { bob_won += 1; (&bob, &alice) };
        assert_no_content(&change_role(&env, winner, loser.account, "admin").await, "the loser comes back");
    }
    println!("Race: 16 rounds of mutual demotion, each ended with exactly 1 administrator, one 204 and one 403 (alice won {alice_won}, bob won {bob_won})");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_a_disable_and_a_deletion_of_the_other_administrator_leave_one() {
    let env = Arc::new(ApiEnv::new());
    let alice = Arc::new(env.admin_fresh("alice"));
    let bob = Arc::new(env.admin_fresh("bob"));
    let (ea, eb) = (env.clone(), env.clone());
    let (a, b) = (alice.clone(), bob.clone());
    let first = tokio::spawn(async move { admin_post(&ea, &a, b.account, "disable").await });
    let (a, b) = (alice.clone(), bob.clone());
    let second = tokio::spawn(async move { delete_account(&eb, &b, a.account).await });
    let (first, second) = (first.await.expect("task"), second.await.expect("task"));
    let admins = env.count_rows("SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND state = 'active'");
    assert_eq!(admins, 1, "{} / {}", first.text(), second.text());
    assert_eq!([first.status.as_u16(), second.status.as_u16()].iter().filter(|s| **s == 204).count(), 1);
}

/// A route and a body that its handler would accept.
fn admin_posts(id: AccountId) -> Vec<(&'static str, String, Value)> {
    admin_changes(id)
        .into_iter()
        .filter(|(_, method, _, _)| *method == Method::POST)
        .map(|(name, _, path, body)| (name, path, body))
        .collect()
}

#[tokio::test]
async fn hostile_bodies_are_refused_as_bad_requests_on_every_route_and_change_nothing() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    env.member("bob");
    let bob = id_of(&env, "bob");
    let before = account_text(&env, bob);
    let lines = env.audit_lines().len();
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let prefix = "{\"role\":\"member\",\"pad\":\"";
    let oversize = format!("{prefix}{}\"}}", "x".repeat(64 * 1024 + 1 - prefix.len() - 2));
    let mut bodies: Vec<(&str, Vec<u8>)> = vec![
        ("broken json", b"{".to_vec()),
        ("an empty body", Vec::new()),
        ("an array", b"[]".to_vec()),
        ("null", b"null".to_vec()),
        ("a string", b"\"role\"".to_vec()),
        ("a number", b"12".to_vec()),
        ("a duplicate member", b"{\"role\":\"member\",\"role\":\"admin\"}".to_vec()),
        ("an unknown member", b"{\"marker\":1}".to_vec()),
        ("a deep list", deep.into_bytes()),
        ("an oversize body", oversize.into_bytes()),
        ("invalid utf-8", vec![b'{', 0xFF, 0xFE, b'}']),
        ("a byte order mark", b"\xEF\xBB\xBF{}".to_vec()),
        ("trailing text", b"{} x".to_vec()),
    ];
    bodies.push(("a nul byte", b"{\"role\":\"mem\0ber\"}".to_vec()));
    let mut routes = admin_posts(bob);
    routes.push(("invitation", "/api/admin/invitations".to_owned(), json!({})));
    let mut refused = 0;
    for (name, path, _) in &routes {
        for (what, bytes) in &bodies {
            let req = edge::Req::new(Method::POST, path)
                .header("host", edge::HOST)
                .header("origin", edge::ORIGIN)
                .header("content-type", "application/json")
                .bytes(bytes.clone())
                .signed(&alice);
            let response = env.send(req).await;
            assert_eq!(response.status, 400, "{name} / {what}: {}", response.text());
            assert_eq!(response.json()["code"], "bad_request", "{name} / {what}");
            assert!(!response.text().contains("marker"), "{name} / {what}: the answer repeats nothing of the input");
            refused += 1;
        }
    }
    // A content type that is not JSON, or twice, is refused before the body.
    for (what, types) in [("text", vec!["text/plain"]), ("form", vec!["application/x-www-form-urlencoded"]), ("twice", vec!["application/json", "application/json"])] {
        let mut req = edge::Req::new(Method::POST, &format!("/api/admin/accounts/{}/disable", bob.get()))
            .header("host", edge::HOST)
            .header("origin", edge::ORIGIN)
            .bytes(b"{}".to_vec())
            .signed(&alice);
        for t in types {
            req = req.header("content-type", t);
        }
        let response = env.send(req).await;
        assert!(matches!(response.status.as_u16(), 400 | 403 | 415), "{what}: {} {}", response.status, response.text());
        refused += 1;
    }
    assert_eq!(account_text(&env, bob), before);
    assert_eq!(env.audit_lines().len(), lines, "nothing was written down");
    assert_eq!(env.invitation_rows(), 0);
    assert!(env.secrets.calls().is_empty() && env.observer.ended().is_empty());
    println!("Hostile: {refused} hostile bodies and content types over {} administration routes, every one refused (400), nothing changed, nothing written", routes.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overload_forty_simultaneous_password_changes_with_one_permit_give_busy_answers() {
    let tuning = Tuning { permits: 1, derive_pause: Some(std::time::Duration::from_millis(50)), ..Tuning::default() };
    let env = Arc::new(ApiEnv::build(Methods::ALL, tuning));
    let bob = Arc::new(env.member_with_password("bob").await);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..40 {
        let (env, bob) = (env.clone(), bob.clone());
        // A new password that breaks the policy: the current password is derived, and then nothing is changed.
        set.spawn(async move { env.send(env.post_json("/api/me/password", &change_body(PASSWORD, "short"), &bob)).await });
    }
    let (mut refused, mut busy, mut other) = (0, 0, Vec::new());
    while let Some(done) = set.join_next().await {
        let response = done.expect("the task finished");
        match response.status.as_u16() {
            400 => refused += 1,
            503 => {
                busy += 1;
                assert_eq!(response.json(), json!({"code": "busy"}));
                assert!(response.all("set-cookie").is_empty());
            }
            status => other.push(status),
        }
    }
    println!("Overload: 40 simultaneous password changes with one permit: 400={refused} 503={busy} other={other:?}");
    assert!(busy >= 6, "one running and 32 waiting leave at least 7 for the busy answer, got {busy}");
    assert!(other.is_empty(), "no other status: {other:?}");
    assert!(env.secrets.calls().is_empty(), "no slot was touched");
    let after = env.send(env.post_json("/api/me/password", &change_body(PASSWORD, NEW_PASSWORD), &bob)).await;
    assert_eq!(after.status, 204, "a change after they finished works: {}", after.text());
}

/// Every stored byte string of an account that must never leave the server, as the texts it could be written as.
fn secret_forms(env: &ApiEnv, id: AccountId) -> Vec<String> {
    let conn = rusqlite::Connection::open(env.store.path()).expect("a second connection");
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for sql in [
        "SELECT verifier FROM password_credentials WHERE account_id = ?1",
        "SELECT salt FROM password_credentials WHERE account_id = ?1",
        "SELECT sealed_secret FROM totp WHERE account_id = ?1",
        "SELECT credential_id FROM passkeys WHERE account_id = ?1",
        "SELECT coalesce(prf_salt, x'00') FROM passkeys WHERE account_id = ?1",
        "SELECT salt FROM unlock_methods WHERE account_id = ?1",
        "SELECT id_hash FROM sessions WHERE account_id = ?1",
    ] {
        let mut stmt = conn.prepare(sql).expect("prepare");
        let rows: Vec<Vec<u8>> =
            stmt.query_map([id.get()], |r| r.get(0)).expect("query").collect::<rusqlite::Result<_>>().expect("rows");
        blobs.extend(rows);
    }
    assert!(blobs.len() >= 7, "the account holds credentials of every kind: {}", blobs.len());
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    let mut forms = Vec::new();
    for blob in blobs.iter().filter(|b| b.len() >= 8) {
        forms.push(blob.iter().map(|b| format!("{b:02x}")).collect::<String>());
        forms.push(URL_SAFE_NO_PAD.encode(blob));
        forms.push(STANDARD_NO_PAD.encode(blob));
    }
    forms
}

#[tokio::test]
async fn disclosure_no_answer_of_the_administration_or_the_account_holds_a_credential_or_a_session_secret() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let bob = env.member_with_password("bob").await;
    let (added, _key) = add_passkey(&env, &bob, "Laptop", Some(&prf_text(7))).await;
    assert_eq!(added.status, 200);
    assert_eq!(set_passphrase(&env, &bob, NEW_PASSPHRASE).await.status, 204);
    let code = replace_recovery(&env, &bob).await.json()["recovery_code"].as_str().expect("a code").to_owned();
    let other = env.another_session("bob");
    let forms = secret_forms(&env, bob.account);
    let mut forbidden: Vec<String> = forms;
    forbidden.extend([PASSWORD.to_owned(), NEW_PASSPHRASE.to_owned(), other.cookie.clone(), bob.cookie.clone(), other.csrf.clone(), bob.csrf.clone()]);
    for word in ["verifier", "sealed", "passkey_json", "id_hash", "prf_salt", "credential_id", "password_hash"] {
        forbidden.push(word.to_owned());
    }

    let mut answers: Vec<(String, String)> = Vec::new();
    let mut note = |what: &str, response: &Resp| answers.push((what.to_owned(), format!("{}\n{:?}", response.text(), response.headers)));
    note("list", &env.send(env.get_as("/api/admin/accounts", &alice)).await);
    note("me of bob", &env.me_of(&bob).await);
    note("me of the other session", &env.me_of(&other).await);
    note("invitation", &env.send(env.post_json("/api/admin/invitations", &json!({"role": "member"}), &alice)).await);
    for (name, method, path, body) in admin_changes(bob.account) {
        if name == "delete" {
            continue;
        }
        note(name, &env.send(request_of(&env, &method, &path, &body, &alice)).await);
    }
    note("a refusal", &env.send(env.post_json("/api/admin/accounts/987654/role", &json!({"role": "admin"}), &alice)).await);
    for (what, text) in &answers {
        for secret in &forbidden {
            assert!(!text.contains(secret.as_str()), "{what} holds {:?}", &secret[..secret.len().min(12)]);
        }
        assert!(!text.contains(&code), "{what} holds the recovery code");
    }
    let audit = env.audit_text();
    for secret in &forbidden {
        if !secret.starts_with(|c: char| c.is_ascii_lowercase()) || secret.len() > 12 {
            assert!(!audit.contains(secret.as_str()), "the audit holds {:?}", &secret[..secret.len().min(12)]);
        }
    }
    assert!(!audit.contains(&code));
    println!("Disclosure: {} answers (list, /api/me, invitation, five changes, a refusal) hold none of {} stored forms of verifier, salts, sealed secret, credential ids, session hashes, cookies, tokens, the passphrase or the recovery code", answers.len(), forbidden.len());
}

const CANARY: &str = "canary-4c1e9a7b-must-not-appear-anywhere";

#[tokio::test]
async fn canary_a_secret_in_every_input_appears_in_no_answer_header_or_audit_line() {
    let env = ApiEnv::new();
    let alice = env.admin_fresh("alice");
    let id = env.person("bob").await;
    env.set_password_text(id, CANARY).await;
    let bob = env.signed_aged("bob", 0);
    let mut seen: Vec<String> = Vec::new();
    let mut note = |response: &Resp| seen.push(format!("{}\n{:?}", response.text(), response.headers));
    let wrong = format!("{CANARY}-wrong");

    note(&env.send(env.post_json("/api/me/password", &change_body(&wrong, &wrong), &bob)).await);
    note(&env.send(env.post_json("/api/me/password", &change_body(CANARY, &format!("{CANARY}{}", "x".repeat(300))), &bob)).await);
    note(&env.send(env.post_json("/api/me/password", &json!({"current": CANARY, "new": CANARY, "x": CANARY}), &bob)).await);
    note(&env.send(env.put_json("/api/me/passphrase", &json!({"passphrase": CANARY, "x": CANARY}), &bob)).await);
    note(&env.send(env.put_json("/api/me/passphrase", &json!({"passphrase": CANARY.repeat(8)}), &bob)).await);
    note(&env.send(env.post_json("/api/me/totp", &json!({"step": CANARY}), &bob)).await);
    note(&env.send(env.post_json("/api/me/totp", &json!({"step": "finish", "code": CANARY}), &bob)).await);
    note(&env.send(env.post_json("/api/me/passkeys", &json!({"step": "begin", "name": CANARY.repeat(3)}), &bob)).await);
    note(&env.send(env.post_json("/api/me/passkeys", &json!({"step": "finish", "credential": {"id": CANARY}, "prf": CANARY}), &bob)).await);
    note(&env.send(env.delete_as(&format!("/api/me/passkeys/{CANARY}"), &bob)).await);
    note(&env.send(env.delete_as(&format!("/api/me/sessions/{CANARY}"), &bob)).await);
    note(&env.send(env.post_json("/api/me/recovery", &json!({"x": CANARY}), &bob)).await);
    note(&env.send(env.post_json(&format!("/api/admin/accounts/{CANARY}/role"), &json!({"role": CANARY}), &alice)).await);
    note(&env.send(env.post_json(&format!("/api/admin/accounts/{}/role", id.get()), &json!({"role": CANARY}), &alice)).await);
    note(&env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "username": CANARY, "display_name": CANARY}), &alice)).await);
    note(&env.send(env.post_json("/api/admin/invitations", &json!({"role": "member", "days": CANARY}), &alice)).await);

    // The canary is the real password: a good change uses it and the secrets it shows are shown to the person only.
    let good = env.send(env.post_json("/api/me/password", &change_body(CANARY, NEW_PASSWORD), &bob)).await;
    assert_eq!(good.status, 204, "{}", good.text());
    let recovery = replace_recovery(&env, &bob).await.json()["recovery_code"].as_str().expect("a code").to_owned();
    let begin = totp_begin(&env, &bob).await;
    let shown = begin.json()["secret"].as_str().expect("a secret").to_owned();
    note(&env.send(env.get_as("/api/admin/accounts", &alice)).await);
    note(&env.me_of(&bob).await);

    for text in &seen {
        assert!(!text.contains(CANARY), "an answer repeats the canary: {text}");
        assert!(!text.contains(&recovery) && !text.contains(&shown), "a secret that was shown once is shown again");
    }
    let audit = env.audit_text();
    assert!(!audit.contains(CANARY) && !audit.contains(&recovery) && !audit.contains(&shown) && !audit.contains(NEW_PASSWORD));
    assert!(!env.credential_dump(id).contains(CANARY), "the stored forms hold nothing readable");
    println!("Canary: {} answers and the whole audit directory hold neither the canary (as password, new password, passphrase, name, code, id, handle, role, days) nor the recovery code nor the authenticator secret shown once", seen.len());
}
