//! Every audited action writes exactly one line.
//!
//! The table has one case per action of the audit vocabulary: the environment that can trigger it, the trigger (a real route
//! call, flow or sweep) and the line it must write. After a trigger the new lines of the audit directory are read and compared
//! with the case: the action, the result, the kind of the actor and of the target, the companion lines the same step writes by
//! design, and a `detail` that holds nothing the step handled secretly.
//!
//! The guards: `every_action_has_a_trigger` compares the vocabulary with the table both ways, and each counting helper has a
//! pure test with a synthetic audit text.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/unlock_env.rs"]
mod unlock_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::collections::BTreeSet;

use holzwave_server::audit::Action;
use holzwave_server::http::app::StoreState;
use serde_json::{Value, json};
use unlock_env::{PASSWORD, Row, UnlockEnv, User};
use edge::Signed;

// ---- the table ---------------------------------------------------------------------------------------------------------------

/// One case: an action of the vocabulary, the environment, the trigger in words, and the line it writes.
struct Case {
    action: &'static str,
    environment: &'static str,
    trigger: &'static str,
    result: &'static str,
    /// `account` or `anonymous`.
    actor: &'static str,
    /// `account` or `none`.
    target: &'static str,
    /// The actions of lines the same step writes by design besides the case's own.
    also: &'static [&'static str],
}

const REAL: &str = "the real routes, the real gate and workers, a manual clock";

const fn case(action: &'static str, trigger: &'static str, result: &'static str, actor: &'static str, target: &'static str, also: &'static [&'static str]) -> Case {
    Case { action, environment: REAL, trigger, result, actor, target, also }
}

/// The table: in the order of the vocabulary.
fn table() -> Vec<Case> {
    vec![
        case("bootstrap.redeem", "the enrolment of the first administrator with the setup token (begin, then the authenticator code)", "ok", "account", "account", &[]),
        case("signin.ok", "a password and an authenticator code", "ok", "account", "none", &[]),
        case("signin.fail", "a wrong password for a known name", "fail", "anonymous", "account", &[]),
        case("signin.locked", "the attempt that finds the account locked after its failures reached the limit", "fail", "anonymous", "account", &[]),
        case("signout", "POST /auth/logout of a person who has a second session", "ok", "account", "none", &[]),
        case("session.expired", "the first lookup of a session after its idle time", "ok", "account", "account", &[]),
        case("session.step_up", "POST /auth/step-up with the password", "ok", "account", "none", &[]),
        case("account.create", "POST /api/admin/invitations with a username", "ok", "account", "account", &[]),
        case("account.invite", "POST /api/admin/invitations without a username", "ok", "account", "none", &[]),
        case("account.redeem", "the enrolment of an invited person (begin, then the authenticator code)", "ok", "account", "account", &[]),
        case("account.disable", "POST /api/admin/accounts/{id}/disable", "ok", "account", "account", &[]),
        case("account.enable", "POST /api/admin/accounts/{id}/enable", "ok", "account", "account", &[]),
        case("account.delete", "DELETE /api/admin/accounts/{id}", "ok", "account", "account", &[]),
        case("account.role", "POST /api/admin/accounts/{id}/role", "ok", "account", "account", &[]),
        case("account.unlock", "POST /api/admin/accounts/{id}/unlock-signin", "ok", "account", "account", &[]),
        case("oidc.create", "the accounts service's single sign-on sign-in for an identity that is not linked", "ok", "account", "account", &[]),
        case("auth.password.change", "POST /api/me/password", "ok", "account", "account", &[]),
        case("auth.totp.enrol", "POST /api/me/totp, begin and finish", "ok", "account", "account", &[]),
        case("auth.passkey.add", "POST /api/me/passkeys, begin and finish, with the software authenticator", "ok", "account", "account", &[]),
        case("auth.passkey.remove", "DELETE /api/me/passkeys/{id}", "ok", "account", "account", &[]),
        case("auth.passphrase.set", "PUT /api/me/passphrase", "ok", "account", "account", &[]),
        case("recovery.use", "POST /auth/unlock with the recovery code of a locked store", "ok", "account", "account", &[]),
        case("recovery.replace", "POST /api/me/recovery", "ok", "account", "account", &[]),
        case("secrets.reset", "POST /api/admin/accounts/{id}/reset-secrets", "ok", "account", "account", &[]),
        case("secrets.unlock", "POST /auth/unlock with the passphrase of a locked store", "ok", "account", "account", &[]),
        case("secrets.lock", "the end of the last session of a person whose store is open", "ok", "account", "account", &["signout"]),
    ]
}

/// What is wrong between the vocabulary and the table: the actions with no case and the cases with no action.
fn table_problems(vocabulary: &[&str], cases: &[&str]) -> Vec<String> {
    let vocabulary_set: BTreeSet<&str> = vocabulary.iter().copied().collect();
    let case_set: BTreeSet<&str> = cases.iter().copied().collect();
    let mut problems = Vec::new();
    for missing in vocabulary_set.difference(&case_set) {
        problems.push(format!("the action {missing} has no trigger"));
    }
    for extra in case_set.difference(&vocabulary_set) {
        problems.push(format!("the trigger {extra} names no action of the vocabulary"));
    }
    if cases.len() != case_set.len() {
        problems.push("an action has two cases".to_owned());
    }
    problems
}

fn vocabulary() -> Vec<&'static str> {
    Action::ALL.iter().map(|a| a.as_str()).collect()
}

#[test]
fn every_action_has_a_trigger() {
    let table = table();
    let actions: Vec<&str> = table.iter().map(|c| c.action).collect();
    let problems = table_problems(&vocabulary(), &actions);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(table.iter().map(|c| c.action).collect::<Vec<_>>(), vocabulary(), "the table follows the order of the vocabulary");
    println!("Guards: the vocabulary has {} actions and the table has {} cases; they name the same actions in the same order", vocabulary().len(), table.len());
}

#[test]
fn negative_control_a_vocabulary_with_one_more_action_and_a_table_with_one_more_case_are_named() {
    let vocabulary = ["a", "b", "c"];
    assert!(table_problems(&vocabulary, &["a", "b", "c"]).is_empty());
    let one_more_action = table_problems(&["a", "b", "c", "d"], &["a", "b", "c"]);
    assert_eq!(one_more_action, ["the action d has no trigger"]);
    let one_more_case = table_problems(&vocabulary, &["a", "b", "c", "z"]);
    assert_eq!(one_more_case, ["the trigger z names no action of the vocabulary"]);
    let twice = table_problems(&vocabulary, &["a", "a", "b", "c"]);
    assert!(twice.iter().any(|p| p.contains("two cases")), "{twice:?}");
}

// ---- counting ----------------------------------------------------------------------------------------------------------------

/// The lines of `after` that `before` did not have: the audit text only grows, so the new lines are the tail.
fn count_new_lines(before: &str, after: &str) -> usize {
    let old = before.lines().filter(|l| !l.trim().is_empty()).count();
    let new = after.lines().filter(|l| !l.trim().is_empty()).count();
    new.saturating_sub(old)
}

#[test]
fn count_new_lines_over_synthetic_text_counts_zero_one_and_two() {
    let base = "{\"action\":\"a\"}\n";
    assert_eq!(count_new_lines(base, base), 0);
    assert_eq!(count_new_lines(base, &format!("{base}{{\"action\":\"b\"}}\n")), 1);
    assert_eq!(count_new_lines(base, &format!("{base}{{\"action\":\"b\"}}\n{{\"action\":\"c\"}}\n")), 2);
    assert_eq!(count_new_lines("", ""), 0);
    assert_eq!(count_new_lines(base, &format!("{base}\n\n")), 0, "empty lines are no lines");
}

/// A running audit of one environment: the rows that are new since the last call.
struct Watch<'a> {
    env: &'a UnlockEnv,
    seen: usize,
    text: String,
}

impl<'a> Watch<'a> {
    fn new(env: &'a UnlockEnv) -> Watch<'a> {
        Watch { env, seen: env.audit_lines().len(), text: env.audit_text() }
    }

    /// The rows written since the last call; the arithmetic of the text and of the rows must agree.
    fn new_rows(&mut self) -> Vec<Row> {
        let rows = self.env.audit_since(self.seen);
        let text = self.env.audit_text();
        assert_eq!(rows.len(), count_new_lines(&self.text, &text), "the rows and the text count the same new lines");
        self.seen += rows.len();
        self.text = text;
        rows
    }
}

fn words(rows: &[Row]) -> Vec<String> {
    rows.iter().map(Row::word).collect()
}

/// The kind of an actor field: an account number, or the word for nobody.
fn actor_kind(actor: &str) -> &'static str {
    if !actor.is_empty() && actor.bytes().all(|b| b.is_ascii_digit()) {
        "account"
    } else if actor == "anonymous" {
        "anonymous"
    } else {
        "unknown"
    }
}

/// The kind of a target field: an account number, or empty.
fn target_kind(target: &str) -> &'static str {
    if target.is_empty() {
        "none"
    } else if target.bytes().all(|b| b.is_ascii_digit()) {
        "account"
    } else {
        "unknown"
    }
}

#[test]
fn negative_control_the_kinds_of_actor_and_target_tell_an_account_from_nobody_and_from_a_wrong_value() {
    assert_eq!((actor_kind("12"), actor_kind("anonymous"), actor_kind("mallory"), actor_kind("")), ("account", "anonymous", "unknown", "unknown"));
    assert_eq!((target_kind("7"), target_kind(""), target_kind("x7")), ("account", "none", "unknown"));
}

/// What the case says about the new rows: exactly its own line with the fixed fields, and its companions, nothing else.
fn check_case(case: &Case, rows: &[Row], canaries: &[String]) {
    let own: Vec<&Row> = rows.iter().filter(|r| r.action == case.action).collect();
    assert_eq!(own.len(), 1, "{}: exactly one line of the action, got {:?}", case.action, words(rows));
    let row = own[0];
    assert_eq!(row.result, case.result, "{}: the result", case.action);
    assert_eq!(actor_kind(&row.actor), case.actor, "{}: the kind of the actor", case.action);
    assert_eq!(target_kind(&row.target), case.target, "{}: the kind of the target", case.action);
    let mut others: Vec<&str> = rows.iter().filter(|r| r.action != case.action).map(|r| r.action.as_str()).collect();
    let mut wanted: Vec<&str> = case.also.to_vec();
    others.sort_unstable();
    wanted.sort_unstable();
    assert_eq!(others, wanted, "{}: the other lines the step writes", case.action);
    let text = rows.iter().map(|r| r.detail.to_string()).collect::<String>();
    for canary in canaries {
        assert!(!text.contains(canary.as_str()), "{}: a detail holds a secret the step handled", case.action);
    }
}

// ---- the people and the triggers ---------------------------------------------------------------------------------------------

struct World {
    env: UnlockEnv,
    alice: User,
    alice_session: Signed,
    bob: User,
    bob_session: Signed,
}

impl World {
    async fn start() -> World {
        let env = UnlockEnv::start().await;
        let (alice, alice_session, bob, bob_session) = env.people().await;
        World { env, alice, alice_session, bob, bob_session }
    }
}

/// The strings a step handled that must not be in any line.
fn secrets_of(users: &[&User]) -> Vec<String> {
    users.iter().flat_map(|u| [u.password.clone(), u.recovery.clone(), hex(&u.secret)]).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fresh administrator session (the step-up window is five minutes).
async fn fresh_admin(w: &World) -> Signed {
    w.env.sign_in(&w.alice).await
}

/// The enrolment through the routes: the begin step, the authenticator code, and the answer with the session cookie.
async fn enrol(env: &UnlockEnv, route: &str, token: &str, name: &str, password: &str) {
    let begin = env
        .post(route, &json!({"step": "begin", "token": token, "username": name, "display_name": name, "method": "password", "password": password}), None)
        .await;
    assert_eq!(begin.status, 200, "begin: {}", begin.text());
    let pending = begin
        .all("set-cookie")
        .iter()
        .find_map(|c| c.strip_prefix("__Host-hw_pending=").map(|r| r.split(';').next().unwrap_or_default().to_owned()))
        .expect("a pending cookie");
    let secret = totp_rs::Secret::try_from_base32(begin.json()["secret"].as_str().expect("a secret").to_owned()).expect("base32").as_bytes().to_vec();
    let code = holzwave_server::auth::totp::code_at(&secret, env.now()).expect("a code").to_string();
    let finish = env.post(route, &json!({"step": "finish", "code": code}), Some(("__Host-hw_pending", &pending))).await;
    assert_eq!(finish.status, 200, "finish: {}", finish.text());
}

/// Runs the trigger of an action in an environment of its own and answers the new rows and the strings to keep out of them.
async fn trigger(action: &str) -> (Vec<Row>, Vec<String>) {
    // The bootstrap case needs an environment with no account.
    if action == "bootstrap.redeem" {
        let env = UnlockEnv::start().await;
        let token = env.accounts.bootstrap_issue().expect("a token");
        drop(token);
        let mut watch = Watch::new(&env);
        let (user, _, _) = env.bootstrap("first", PASSWORD).await;
        let rows = watch.new_rows();
        // The issue of the token is part of `bootstrap`; the lines of the enrolment are what counts.
        return (rows, secrets_of(&[&user]));
    }
    let w = World::start().await;
    let env = &w.env;
    let mut watch = Watch::new(env);
    let keep = secrets_of(&[&w.alice, &w.bob]);
    match action {
        "signin.ok" => {
            env.sign_in(&w.bob).await;
        }
        "signin.fail" => {
            let r = env.post("/auth/password", &json!({"username": w.bob.name, "password": "a wrong horse battery"}), None).await;
            assert_eq!(r.status, 401);
        }
        "signin.locked" => {
            let mut locked = false;
            for _ in 0..12 {
                watch.new_rows();
                let r = env.post("/auth/password", &json!({"username": w.bob.name, "password": "a wrong horse battery"}), None).await;
                assert!(r.status == 401 || r.status == 429, "{}", r.status);
                let rows = env.audit_since(0);
                if rows.iter().any(|row| row.action == "signin.locked") {
                    locked = true;
                    break;
                }
            }
            assert!(locked, "the failures end in a locked line");
        }
        "signout" => {
            let second = env.sign_in(&w.bob).await;
            watch.new_rows();
            let r = env.sign_out(&second).await;
            assert_eq!(r.status, 204);
        }
        "session.expired" => {
            env.advance(1801);
            let r = env.api(&w.bob_session, "GET", "/api/me", None).await;
            assert_eq!(r.status, 401);
        }
        "session.step_up" => {
            let r = env.api(&w.bob_session, "POST", "/auth/step-up", Some(&json!({"password": w.bob.password}))).await;
            assert_eq!(r.status, 200, "{}", r.text());
        }
        "account.create" => {
            let admin = fresh_admin(&w).await;
            watch.new_rows();
            let r = env.api(&admin, "POST", "/api/admin/invitations", Some(&json!({"role": "member", "days": 1, "username": "carol"}))).await;
            assert_eq!(r.status, 200, "{}", r.text());
        }
        "account.invite" => {
            let admin = fresh_admin(&w).await;
            watch.new_rows();
            let r = env.api(&admin, "POST", "/api/admin/invitations", Some(&json!({"role": "member", "days": 1}))).await;
            assert_eq!(r.status, 200, "{}", r.text());
        }
        "account.redeem" => {
            // The invitation is issued first (its own line, `account.invite`); the enrolment through the routes follows.
            let token = env.accounts.invite_create(w.alice.id, None, None, holzwave_server::store::Role::Member).expect("an invitation").token.expose().to_owned();
            watch.new_rows();
            enrol(env, "/auth/invite/redeem", &token, "carol", "yet another horse battery").await;
        }
        "account.disable" | "account.enable" | "account.delete" | "account.role" | "account.unlock" | "secrets.reset" => {
            let admin = fresh_admin(&w).await;
            let id = w.bob.id.get();
            if action == "account.enable" {
                let r = env.act(&admin, "POST", &format!("/api/admin/accounts/{id}/disable")).await;
                assert_eq!(r.status, 204, "{}", r.text());
            }
            watch.new_rows();
            let (method, path, body) = match action {
                "account.disable" => ("POST", format!("/api/admin/accounts/{id}/disable"), json!({})),
                "account.enable" => ("POST", format!("/api/admin/accounts/{id}/enable"), json!({})),
                "account.delete" => ("DELETE", format!("/api/admin/accounts/{id}"), json!({})),
                "account.role" => ("POST", format!("/api/admin/accounts/{id}/role"), json!({"role": "admin"})),
                "account.unlock" => ("POST", format!("/api/admin/accounts/{id}/unlock-signin"), json!({})),
                _ => ("POST", format!("/api/admin/accounts/{id}/reset-secrets"), json!({})),
            };
            let r = env.api(&admin, method, &path, Some(&body)).await;
            assert_eq!(r.status, 204, "{action}: {}", r.text());
        }
        "oidc.create" => {
            use holzwave_server::accounts::{OidcClaims, ProviderRules};
            let claims = OidcClaims {
                issuer: "https://idp.example.test".to_owned(),
                subject: "subject-1".to_owned(),
                email: Some("dana@example.org".to_owned()),
                email_verified: true,
                preferred_username: Some("dana".to_owned()),
                display_name: None,
                groups: Vec::new(),
            };
            let rules = ProviderRules { allowed_domains: vec!["example.org".to_owned()], allowed_groups: Vec::new() };
            env.accounts.oidc_sign_in(&claims, &rules, None).expect("a new account");
        }
        "auth.password.change" => {
            let r = env.api(&w.bob_session, "POST", "/api/me/password", Some(&json!({"current": w.bob.password, "new": "an even newer horse battery"}))).await;
            assert_eq!(r.status, 204, "{}", r.text());
        }
        "auth.totp.enrol" => {
            let begin = env.api(&w.bob_session, "POST", "/api/me/totp", Some(&json!({"step": "begin"}))).await;
            assert_eq!(begin.status, 200, "{}", begin.text());
            let secret = totp_rs::Secret::try_from_base32(begin.json()["secret"].as_str().expect("a secret").to_owned()).expect("base32").as_bytes().to_vec();
            env.advance(unlock_env::CODE_STEP);
            let code = holzwave_server::auth::totp::code_at(&secret, env.now()).expect("a code").to_string();
            let finish = env.api(&w.bob_session, "POST", "/api/me/totp", Some(&json!({"step": "finish", "code": code}))).await;
            assert_eq!(finish.status, 204, "{}", finish.text());
        }
        "auth.passkey.add" | "auth.passkey.remove" => {
            let begin = env.api(&w.bob_session, "POST", "/api/me/passkeys", Some(&json!({"step": "begin", "name": "phone"}))).await;
            assert_eq!(begin.status, 200, "{}", begin.text());
            let mut authenticator = unlock_env::webauthn_client::SoftPasskey::new();
            let origin = webauthn_rs::prelude::Url::parse(edge::ORIGIN).expect("the origin");
            let credential = authenticator.register(&begin.json()["options"], &origin).expect("the authenticator answers");
            let finish = env.api(&w.bob_session, "POST", "/api/me/passkeys", Some(&json!({"step": "finish", "credential": credential}))).await;
            assert_eq!(finish.status, 200, "{}", finish.text());
            if action == "auth.passkey.remove" {
                let id = finish.json()["id"].as_i64().map(|i| i.to_string()).or_else(|| finish.json()["id"].as_str().map(str::to_owned)).expect("an id");
                watch.new_rows();
                let r = env.act(&w.bob_session, "DELETE", &format!("/api/me/passkeys/{id}")).await;
                assert_eq!(r.status, 204, "{} (id {id}, finish answered {})", r.text(), finish.text());
            }
        }
        "auth.passphrase.set" => {
            let r = env.api(&w.bob_session, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": "a long passphrase for my store"}))).await;
            assert_eq!(r.status, 204, "{}", r.text());
        }
        "recovery.replace" => {
            let r = env.api(&w.bob_session, "POST", "/api/me/recovery", Some(&json!({}))).await;
            assert_eq!(r.status, 200, "{}", r.text());
        }
        "secrets.unlock" | "recovery.use" => {
            if action == "secrets.unlock" {
                let r = env.api(&w.bob_session, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": "a long passphrase for my store"}))).await;
                assert_eq!(r.status, 204, "{}", r.text());
            }
            env.lock_at_the_worker(&w.bob).await;
            env.wait_state(&w.bob, StoreState::Locked, "the store to lock").await;
            watch.new_rows();
            let r = if action == "secrets.unlock" {
                env.unlock_passphrase(&w.bob_session, "a long passphrase for my store").await
            } else {
                env.unlock_recovery(&w.bob_session, &w.bob.recovery).await
            };
            assert_eq!(r.status, 200, "{}", r.text());
        }
        "secrets.lock" => {
            let r = env.sign_out(&w.bob_session).await;
            assert_eq!(r.status, 204, "{}", r.text());
            env.eventually("the lock line", || env.lock_lines() == 1).await;
        }
        other => panic!("no trigger for {other}"),
    }
    (watch.new_rows(), keep)
}

#[test]
fn audit_once_every_action_of_the_vocabulary_writes_exactly_its_one_line() {
    on_big_stack(async {
        let table = table();
        let mut printed = Vec::new();
        for case in &table {
            let (rows, secrets) = trigger(case.action).await;
            println!("  {:<22} {:?}", case.action, rows.iter().map(|r| format!("{}/{} actor={} target={} {}", r.action, r.result, r.actor, r.target, r.detail)).collect::<Vec<_>>());
            check_case(case, &rows, &secrets);
            printed.push(format!("{}: {} line(s) in all, the case's own once ({}; {}; {}, actor {}, target {})", case.action, rows.len(), case.environment, case.trigger, case.result, case.actor, case.target));
        }
        println!("Audit once: {} cases, one per action of the vocabulary ({} actions)", table.len(), vocabulary().len());
        for line in &printed {
            println!("  {line}");
        }
    });
}

/// Runs a test body on a thread with a big stack: the futures of the environments are large in a debug build.
fn on_big_stack<F: std::future::Future<Output = ()> + Send + 'static>(body: F) {
    let handle = std::thread::Builder::new()
        .name("big-stack".to_owned())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).thread_stack_size(64 * 1024 * 1024).enable_all().build().expect("a runtime");
            runtime.block_on(body);
        })
        .expect("a thread");
    if let Err(panic) = handle.join() {
        std::panic::resume_unwind(panic);
    }
}

// ---- the enrolment, the expiry, the concurrency ------------------------------------------------------------------------------

#[test]
fn enrolment_lines_one_line_and_no_sign_in_or_session_line_though_the_enrolment_starts_a_session() {
    on_big_stack(async {
        // The first administrator.
        let env = UnlockEnv::start().await;
        let mut watch = Watch::new(&env);
        let (alice, session, _) = env.bootstrap("alice", PASSWORD).await;
        let rows = watch.new_rows();
        assert_eq!(words(&rows), ["bootstrap.redeem/ok"], "the bootstrap enrolment writes one line");
        assert_eq!(env.live_sessions(&alice), 1, "the enrolment started a session");
        assert!(env.ws.test.sessions.lookup(&session.cookie, env.now()).is_ok());
        // An invited person.
        let token = env.accounts.invite_create(alice.id, None, None, holzwave_server::store::Role::Member).expect("an invitation").token.expose().to_owned();
        watch.new_rows();
        enrol(&env, "/auth/invite/redeem", &token, "bob", "another horse battery").await;
        let rows = watch.new_rows();
        assert_eq!(words(&rows), ["account.redeem/ok"], "the invitation enrolment writes one line");
        let bob = env.ws.test.store.account_by_username("bob").expect("read").expect("bob");
        assert_eq!(env.ws.test.sessions.list_live(bob.id, env.now()).expect("sessions").len(), 1, "the enrolment started a session");
        let all = env.audit_lines();
        assert!(!all.iter().any(|r| r.action == "signin.ok" || r.action == "session.step_up"), "no sign-in line follows an enrolment: {:?}", words(&all));
        println!("Enrolment lines: bootstrap.redeem once and account.redeem once; each enrolment started one session and wrote no signin.ok and no session line (lines overall: {:?})", words(&all));
    });
}

#[test]
fn expired_a_session_writes_one_line_at_the_first_lookup_after_the_idle_time_and_none_at_the_second() {
    on_big_stack(async {
        let w = World::start().await;
        let mut watch = Watch::new(&w.env);
        // A lookup inside the idle time writes nothing.
        w.env.advance(600);
        assert_eq!(w.env.api(&w.bob_session, "GET", "/api/me", None).await.status, 200);
        assert!(watch.new_rows().is_empty(), "a live lookup writes nothing");
        // Past the idle time (30 minutes since the last use).
        w.env.advance(1801);
        let first = w.env.api(&w.bob_session, "GET", "/api/me", None).await;
        assert_eq!(first.status, 401);
        let rows = watch.new_rows();
        // The end of a person's last session locks the store (its own line, once per person, and the target tells which).
        let expired: Vec<&Row> = rows.iter().filter(|r| r.action == "session.expired").collect();
        assert_eq!(expired.len(), 1, "the first lookup after the idle time writes one session.expired line: {:?}", words(&rows));
        assert_eq!((expired[0].actor.as_str(), expired[0].target.as_str()), (w.bob.id.get().to_string().as_str(), w.bob.id.get().to_string().as_str()), "for bob");
        let locks: Vec<&str> = rows.iter().filter(|r| r.action != "session.expired").map(|r| r.action.as_str()).collect();
        assert!(locks.iter().all(|a| *a == "secrets.lock"), "only locks follow: {:?}", words(&rows));
        let lock_targets: BTreeSet<&str> = rows.iter().filter(|r| r.action == "secrets.lock").map(|r| r.target.as_str()).collect();
        assert_eq!(lock_targets.len(), locks.len(), "one lock line for each person: {:?}", words(&rows));
        let second = w.env.api(&w.bob_session, "GET", "/api/me", None).await;
        assert_eq!(second.status, 401);
        // A lock line of a store may still arrive from the gate (it works on its own task); the lookup itself writes none.
        let later = watch.new_rows();
        assert!(later.iter().all(|r| r.action == "secrets.lock"), "the second lookup of the same session writes no line of its own: {:?}", words(&later));
        assert_eq!(w.env.audit_of("session.expired").len(), 1, "one session.expired line in all");
        println!("Expired: a lookup 10 minutes in wrote nothing; the first lookup 30 minutes after the last use wrote {:?} (one session.expired for bob, a lock line for each person whose last session ended); the second lookup wrote nothing", words(&rows));
    });
}

#[test]
fn concurrent_triggers_of_one_action_write_one_line_each() {
    on_big_stack(async {
        let w = World::start().await;
        let admin = fresh_admin(&w).await;
        let mut watch = Watch::new(&w.env);
        // Two invitations at once, and two failed sign-ins for two different names at once.
        let body = json!({"role": "member", "days": 1});
        let (a, b) = tokio::join!(w.env.api(&admin, "POST", "/api/admin/invitations", Some(&body)), w.env.api(&admin, "POST", "/api/admin/invitations", Some(&body)));
        assert_eq!((a.status, b.status), (200, 200));
        let rows = watch.new_rows();
        assert_eq!(words(&rows), ["account.invite/ok", "account.invite/ok"], "two invitations, two lines");
        let wrong = |name: &str| json!({"username": name, "password": "a wrong horse battery"});
        let (for_alice, for_bob) = (wrong(&w.alice.name), wrong(&w.bob.name));
        let (c, d) = tokio::join!(w.env.post("/auth/password", &for_alice, None), w.env.post("/auth/password", &for_bob, None));
        assert_eq!((c.status, d.status), (401, 401));
        let rows = watch.new_rows();
        assert_eq!(words(&rows), ["signin.fail/fail", "signin.fail/fail"], "two failures, two lines");
        println!("Audit once (concurrent): two invitations at once wrote two account.invite lines, two failed sign-ins wrote two signin.fail lines");
    });
}

// ---- refusals ----------------------------------------------------------------------------------------------------------------

/// A refusal: what to do, and the failure line it may write (`None`: no line at all).
async fn refusal(name: &str) -> (Vec<Row>, Option<&'static str>) {
    let w = World::start().await;
    let env = &w.env;
    let mut watch = Watch::new(env);
    let allowed = match name {
        "a wrong password" => {
            let r = env.post("/auth/password", &json!({"username": w.bob.name, "password": "a wrong horse battery"}), None).await;
            assert_eq!(r.status, 401);
            Some("signin.fail")
        }
        "a wrong authenticator code" => {
            let begin = env.post("/auth/password", &json!({"username": w.bob.name, "password": w.bob.password}), None).await;
            assert_eq!(begin.status, 200);
            let pending = begin.all("set-cookie").iter().find_map(|c| c.strip_prefix("__Host-hw_pending=").map(|r| r.split(';').next().unwrap_or_default().to_owned())).expect("pending");
            watch.new_rows();
            let r = env.post("/auth/totp", &json!({"code": "000000"}), Some(("__Host-hw_pending", &pending))).await;
            assert_eq!(r.status, 401);
            Some("signin.fail")
        }
        "a stale administrator on an administrator route" => {
            env.advance(360);
            let r = env.act(&w.alice_session, "POST", &format!("/api/admin/accounts/{}/disable", w.bob.id.get())).await;
            assert_eq!(r.status, 403);
            None
        }
        "a member on an administrator route" => {
            let r = env.act(&w.bob_session, "POST", &format!("/api/admin/accounts/{}/disable", w.alice.id.get())).await;
            assert_eq!(r.status, 403);
            None
        }
        "a taken name" => {
            let admin = fresh_admin(&w).await;
            watch.new_rows();
            let r = env.api(&admin, "POST", "/api/admin/invitations", Some(&json!({"role": "member", "days": 1, "username": "bob"}))).await;
            assert!(r.status == 409 || r.status == 400, "{} {}", r.status, r.text());
            Some("account.create")
        }
        "a bad invitation token" => {
            let r = env
                .post("/auth/invite/redeem", &json!({"step": "begin", "token": "hwi_0000000000000000000000000000000000000000000", "username": "eve", "display_name": "eve", "method": "password", "password": "another horse battery"}), None)
                .await;
            assert!(r.status >= 400, "{}", r.status);
            Some("account.redeem")
        }
        "a wrong current password" => {
            let r = env.api(&w.bob_session, "POST", "/api/me/password", Some(&json!({"current": "a wrong horse battery", "new": "an even newer horse battery"}))).await;
            assert!(r.status >= 400, "{}", r.status);
            // The current password is checked by the sign-in code, which writes its own failure line.
            Some("signin.fail")
        }
        "a wrong recovery code" => {
            env.lock_at_the_worker(&w.bob).await;
            env.wait_state(&w.bob, StoreState::Locked, "the store to lock").await;
            watch.new_rows();
            let r = env.unlock_recovery(&w.bob_session, "AAAA-AAAA-AAAA-AAAA-AAAA-AAAA").await;
            assert!(r.status >= 400, "{} {}", r.status, r.text());
            Some("recovery.use")
        }
        other => panic!("no refusal named {other}"),
    };
    (watch.new_rows(), allowed)
}

#[test]
fn refusals_write_no_ok_line_and_at_most_the_one_failure_line_the_route_documents() {
    on_big_stack(async {
        let names = [
            "a wrong password",
            "a wrong authenticator code",
            "a stale administrator on an administrator route",
            "a member on an administrator route",
            "a taken name",
            "a bad invitation token",
            "a wrong current password",
            "a wrong recovery code",
        ];
        let mut seen = Vec::new();
        for name in names {
            let (rows, allowed) = refusal(name).await;
            assert!(!rows.iter().any(|r| r.result == "ok"), "{name}: a refusal writes no ok line: {:?}", words(&rows));
            match allowed {
                None => assert!(rows.is_empty(), "{name}: a refusal at a gate writes no line, got {:?}", words(&rows)),
                Some(action) => {
                    assert!(rows.len() <= 1, "{name}: at most one line, got {:?}", words(&rows));
                    assert!(rows.iter().all(|r| r.action == action && r.result == "fail"), "{name}: the line is {action}/fail, got {:?}", words(&rows));
                }
            }
            seen.push(format!("{name}: {:?}", words(&rows)));
        }
        println!("Refusals: {} refusals; none wrote an ok line", seen.len());
        for line in &seen {
            println!("  {line}");
        }
    });
}

#[test]
fn negative_control_check_case_reports_no_line_two_lines_a_wrong_field_a_stray_line_and_a_secret() {
    let row = |action: &str, result: &str, actor: &str, target: &str, detail: Value| Row {
        action: action.to_owned(),
        result: result.to_owned(),
        actor: actor.to_owned(),
        target: target.to_owned(),
        detail,
    };
    let own = case("account.role", "x", "ok", "account", "account", &[]);
    let good = [row("account.role", "ok", "1", "2", json!({"role": "admin"}))];
    check_case(&own, &good, &["secret-value".to_owned()]);
    let fails = |rows: Vec<Row>, canaries: Vec<String>| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_case(&own, &rows, &canaries))).is_err();
    assert!(fails(vec![], vec![]), "no line");
    assert!(fails(vec![good[0].clone(), good[0].clone()], vec![]), "two lines");
    assert!(fails(vec![row("account.role", "fail", "1", "2", json!({}))], vec![]), "a wrong result");
    assert!(fails(vec![row("account.role", "ok", "anonymous", "2", json!({}))], vec![]), "a wrong actor kind");
    assert!(fails(vec![row("account.role", "ok", "1", "", json!({}))], vec![]), "a wrong target kind");
    assert!(fails(vec![good[0].clone(), row("signin.ok", "ok", "1", "", json!({}))], vec![]), "a stray line");
    assert!(fails(vec![row("account.role", "ok", "1", "2", json!({"role": "secret-value"}))], vec!["secret-value".to_owned()]), "a secret in a detail");
}
