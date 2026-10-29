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
use auth_env::{WRONG, set_cookie_value};
use holzwave_server::clock::{Clock as _, from_unix, rfc3339};
use holzwave_server::store::{AccountState, Role};
use serde_json::{Value, json};

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
