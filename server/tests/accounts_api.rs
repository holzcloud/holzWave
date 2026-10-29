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
use holzwave_server::auth::password::params_text;
use holzwave_server::auth::session::parse_token;
use holzwave_server::auth::totp;
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::http::routes_me::SecretsError;
use holzwave_server::store::{AccountId, AccountState, PasswordCredential, Role, UnlockKind};
use holzwave_server::unlock::{kek_from_passphrase, kek_from_prf, kek_from_recovery, parse_recovery_code};
use serde_json::{Value, json};
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
