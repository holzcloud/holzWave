//! The key of a person's secret store lives exactly as long as a session of the person: the lock policy, the store made at
//! sign-in, enrolment, recovery, the administrator's reset and the audit lines, with the real gate, the real worker and the
//! real routes behind a real TLS listener.
//!
//! The tracer tests come first; the filter `tracer` selects exactly them. Everything here needs the `in-process-workers`
//! feature, and the page must have been built (`npm --prefix app run build`) because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/unlock_env.rs"]
mod unlock_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::{Duration, Instant};

use holzwave_server::http::app::StoreState;
use serde_json::json;
use unlock_env::{PASSWORD, Region, UnlockEnv, ai_has_key, ai_key_set, ai_profile, prf_text};
use ws_env::{PATIENCE, focus};

const CANARY_KEY: &str = "sk-ant-tracer-canary-31415";

// ---- the tracer ---------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_secret_survives_the_lock_after_the_last_sign_out_and_is_read_back_after_the_next_sign_in() {
    let env = UnlockEnv::start().await;
    let (alice, session_one, _bob, _) = env.people().await;
    assert_eq!(env.state(&alice), StoreState::Open, "the enrolment leaves the store open");
    let mut socket = env.socket(&session_one).await;
    let first = socket.read_json(PATIENCE).await.expect("the first message");
    assert_eq!((first["event"].as_str(), first["payload"]["state"].as_str()), (Some("hw-session"), Some("ok")), "{first}");

    // A pane that keeps running through all of this, with a variable only this shell knows.
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string()).await;
    socket.ok("text", &json!({"pane": pane, "text": "KEPT=still-here\r"}).to_string()).await;

    let profile = ai_profile(&mut socket).await;
    ai_key_set(&mut socket, &profile, CANARY_KEY).await.expect("the key is stored while the store is open");
    assert_eq!(ai_has_key(&mut socket, &profile).await, Ok(true));
    let stats_before = env.ws.supervisor.account_stats(alice.key);
    assert!(stats_before.worker_running);

    // The last session ends: within a second the store is locked, and one `secrets.lock` line says why.
    let mark = env.audit_lines().len();
    let ended = Instant::now();
    assert_eq!(env.sign_out(&session_one).await.status, 204);
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    let to_locked = ended.elapsed();
    assert!(to_locked < Duration::from_secs(1), "locked {to_locked:?} after the sign-out");
    let rows = env.audit_since(mark);
    let words = sorted_words(&rows);
    assert_eq!(words, ["secrets.lock/ok", "signout/ok"], "{rows:?}");
    let lock = rows.iter().find(|r| r.action == "secrets.lock").expect("the lock line");
    assert_eq!(lock.detail_text("reason").as_deref(), Some("signout"));
    assert_eq!((lock.actor.as_str(), lock.target.as_str()), (alice.id.to_string().as_str(), alice.id.to_string().as_str()));

    // A command that reaches the worker without any session finds the store locked, with the fixed sentence.
    let answer = env.direct(&alice, "ai_has_key", &json!({"profile": profile, "slot": "provider"})).await.expect("attached");
    assert_eq!(UnlockEnv::fault_text(&answer).as_deref(), Some("secrets are locked"), "{answer}");

    // The next sign-in opens the store again, and the secret written before the lock is there.
    let session_two = env.sign_in(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Open);
    let mut again = env.socket(&session_two).await;
    let first = again.read_json(PATIENCE).await.expect("the first message");
    assert_eq!(first["payload"]["state"], "ok", "{first}");
    assert_eq!(ai_has_key(&mut again, &profile).await, Ok(true), "the secret written before the lock is read back");

    // Neither the worker nor the pane was restarted: the same shell still knows its variable.
    assert!(env.ws.supervisor.account_stats(alice.key).worker_running);
    again.ok("text", &json!({"pane": pane, "text": "printf 'hw-%s\\n' $KEPT\r"}).to_string()).await;
    let seen = again.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains("hw-still-here")).await;
    assert!(seen.is_some(), "the pane started before the sign-out still runs");

    // One lock line in all, and the canary key is in no audit line.
    assert_eq!(env.audit_of("secrets.lock").len(), 1);
    assert!(!env.audit_text().contains(CANARY_KEY));
    println!(
        "tracer: open after enrolment -> locked {to_locked:?} after the sign-out (rows {words:?}) -> sign-in -> open, secret read back"
    );
}

/// How long the tests give the gate to act on an announcement that must have an effect.
const SECOND: Duration = Duration::from_secs(1);

/// How long a test waits before it says that something did not happen.
const QUIET: Duration = Duration::from_millis(400);

fn words(rows: &[unlock_env::Row]) -> Vec<String> {
    rows.iter().map(unlock_env::Row::word).collect()
}

/// The same, in alphabetical order: the line of a sign-out and the line of the lock it causes are written by two owners at
/// about the same time, and the order between them is not part of the contract.
fn sorted_words(rows: &[unlock_env::Row]) -> Vec<String> {
    let mut all = words(rows);
    all.sort();
    all
}

// ---- the last session ---------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn last_session_two_browsers_the_store_stays_open_until_the_second_one_ends() {
    let env = UnlockEnv::start().await;
    let (alice, one, _bob, _) = env.people().await;
    let two = env.sign_in(&alice).await;
    assert_eq!(env.live_sessions(&alice), 2);
    let mut socket_two = env.socket(&two).await;
    let profile = ai_profile(&mut socket_two).await;

    let mark = env.audit_lines().len();
    assert_eq!(env.sign_out(&one).await.status, 204);
    tokio::time::sleep(QUIET).await;
    assert_eq!(env.state(&alice), StoreState::Open, "another session of the person is live");
    assert_eq!(words(&env.audit_since(mark)), ["signout/ok"], "no lock line while a session is live");
    ai_key_set(&mut socket_two, &profile, "sk-two-browsers").await.expect("a command works on the other browser");

    let ended = Instant::now();
    assert_eq!(env.sign_out(&two).await.status, 204);
    env.wait_state(&alice, StoreState::Locked, "the second sign-out").await;
    assert!(ended.elapsed() < SECOND, "locked {:?} after the sign-out", ended.elapsed());
    let rows = env.audit_since(mark);
    assert_eq!(sorted_words(&rows), ["secrets.lock/ok", "signout/ok", "signout/ok"], "{rows:?}");
    println!("last session: two browsers, one sign-out leaves it open, the second locks in {:?}", ended.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_sign_out_everywhere_writes_one_lock_line() {
    let env = UnlockEnv::start().await;
    let (alice, one, _bob, _) = env.people().await;
    let _two = env.sign_in(&alice).await;
    let mark = env.audit_lines().len();
    let ended = Instant::now();
    assert_eq!(env.act(&one, "DELETE", "/api/me/sessions").await.status, 204);
    env.wait_state(&alice, StoreState::Locked, "sign out everywhere").await;
    assert!(ended.elapsed() < SECOND);
    tokio::time::sleep(QUIET).await;
    let rows = env.audit_since(mark);
    assert_eq!(sorted_words(&rows), ["secrets.lock/ok", "signout/ok"], "{rows:?}");
    assert_eq!(rows.iter().find(|r| r.action == "secrets.lock").expect("lock").detail_text("reason").as_deref(), Some("signout"));
    println!("last session: sign out everywhere locked in {:?} with one line", ended.elapsed());
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_the_idle_time_running_out_locks_at_the_next_sweep_with_the_cause_expired() {
    let env = UnlockEnv::start().await;
    let (alice, _one, _) = env.bootstrap("alice", PASSWORD).await;
    let mark = env.audit_lines().len();
    env.advance(edge::IDLE);
    assert_eq!(env.sweep().await, 0, "the last second of the idle time is still inside it");
    assert_eq!(env.state(&alice), StoreState::Open);
    env.advance(1);
    assert_eq!(env.sweep().await, 1);
    assert_eq!(env.state(&alice), StoreState::Locked);
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["secrets.lock/ok"], "{rows:?}");
    assert_eq!(rows[0].detail_text("reason").as_deref(), Some("expired"));
    assert_eq!(env.sweep().await, 0, "a second sweep finds nothing");
    assert_eq!(env.audit_since(mark).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_a_session_kept_alive_in_steps_below_the_idle_time_does_not_lock() {
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    let mut session = one.session.clone();
    for _ in 0..4 {
        env.advance(edge::IDLE - 300);
        env.ws.test.sessions.touch(&mut session, env.now()).expect("the session is alive");
        assert_eq!(env.sweep().await, 0);
        assert_eq!(env.state(&alice), StoreState::Open);
    }
    assert_eq!(env.lock_lines(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_the_absolute_time_ends_it_however_recent_the_touch() {
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    let started = one.session.created_at;
    let mut session = one.session.clone();
    while env.now() + 1500 < started + edge::ABSOLUTE {
        env.advance(1500);
        env.ws.test.sessions.touch(&mut session, env.now()).expect("alive");
    }
    env.ws.test.clock.set(started + edge::ABSOLUTE);
    env.ws.test.sessions.touch(&mut session, env.now()).expect("alive on the last second");
    assert_eq!(env.sweep().await, 0, "valid on the last second");
    env.advance(1);
    assert_eq!(env.sweep().await, 1, "one second later it is over, whatever the touch");
    assert_eq!(env.state(&alice), StoreState::Locked);
    let rows = env.audit_of("secrets.lock");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].detail_text("reason").as_deref(), Some("expired"));
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_disabling_the_account_locks_it_with_the_cause_disabled() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, _) = env.people().await;
    let mark = env.audit_lines().len();
    let done = env.act(&admin, "POST", &format!("/api/admin/accounts/{}/disable", bob.id.get())).await;
    assert_eq!(done.status, 204, "{}", done.text());
    // The worker of a disabled account is ended by the supervisor; the sweep records that its key went with it.
    let deadline = Instant::now() + PATIENCE;
    while env.lock_lines() == 0 {
        env.sweep().await;
        assert!(Instant::now() < deadline, "the lock line of the disabled account");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for _ in 0..3 {
        env.sweep().await;
    }
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["account.disable/ok", "secrets.lock/ok"], "{rows:?}");
    assert_eq!(rows[1].detail_text("reason").as_deref(), Some("disabled"));
    assert_eq!(rows[1].target, bob.id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_deleting_the_account_locks_it_with_the_cause_disabled() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, _) = env.people().await;
    let mark = env.audit_lines().len();
    let done = env.act(&admin, "DELETE", &format!("/api/admin/accounts/{}", bob.id.get())).await;
    assert_eq!(done.status, 204, "{}", done.text());
    let deadline = Instant::now() + PATIENCE;
    while env.lock_lines() == 0 {
        env.sweep().await;
        assert!(Instant::now() < deadline, "the lock line of the deleted account");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for _ in 0..3 {
        env.sweep().await;
    }
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["account.delete/ok", "secrets.lock/ok"], "{rows:?}");
    assert_eq!(rows[1].detail_text("reason").as_deref(), Some("disabled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_a_sweep_over_a_locked_or_absent_store_writes_nothing() {
    let env = UnlockEnv::start().await;
    let (alice, admin, bob, _) = env.people().await;
    // Alice signs out and is locked; bob's store is reset and so absent.
    env.sign_out(&admin).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    let again = env.sign_in(&alice).await;
    assert_eq!(env.reset_secrets(&again, &bob).await.status, 204);
    env.sign_out(&again).await;
    env.wait_state(&alice, StoreState::Locked, "the second sign-out").await;
    let mark = env.audit_lines().len();
    env.advance(edge::IDLE * 2);
    assert_eq!(env.sweep().await, 0);
    assert_eq!(env.sweep().await, 0);
    assert!(env.audit_since(mark).is_empty(), "{:?}", env.audit_since(mark));
    assert!(env.store_sha256(&bob).is_none(), "the sweep made no store");
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_the_pane_keeps_running_through_the_lock_and_a_key_can_be_set_after_the_next_sign_in() {
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    let mut socket = env.socket(&one).await;
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string()).await;
    socket.ok("text", &json!({"pane": pane, "text": "MARK=survived\r"}).to_string()).await;
    drop(socket);
    env.advance(edge::IDLE + 1);
    assert_eq!(env.sweep().await, 1);
    assert_eq!(env.state(&alice), StoreState::Locked);
    assert!(env.ws.supervisor.account_stats(alice.key).worker_running, "the lock does not end the worker");

    let two = env.sign_in(&alice).await;
    let mut again = env.socket(&two).await;
    again.ok("text", &json!({"pane": pane, "text": "printf 'hw-%s\\n' $MARK\r"}).to_string()).await;
    assert!(again.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains("hw-survived")).await.is_some());
    let profile = ai_profile(&mut again).await;
    assert_eq!(ai_key_set(&mut again, &profile, "sk-after-the-lock").await, Ok(()));
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_two_sweeps_at_once_write_one_line() {
    let env = UnlockEnv::start().await;
    let (alice, _one, _) = env.bootstrap("alice", PASSWORD).await;
    for round in 0..16 {
        let _session = env.sign_in(&alice).await;
        assert_eq!(env.state(&alice), StoreState::Open, "round {round}");
        env.advance(edge::IDLE + 1);
        let before = env.lock_lines();
        let (a, b) = tokio::join!(env.sweep(), env.sweep());
        assert_eq!(a + b, 1, "round {round}: one of the two sweeps wiped the key");
        assert_eq!(env.lock_lines(), before + 1, "round {round}");
        assert_eq!(env.state(&alice), StoreState::Locked);
    }
}

// ---- the race -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn race_a_sign_in_against_a_sign_out_never_ends_with_a_live_session_and_a_locked_store() {
    let env = std::sync::Arc::new(UnlockEnv::start().await);
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    let alice = std::sync::Arc::new(alice);
    let mut previous = std::sync::Arc::new(first);
    let (mut ended_open, mut ended_locked, mut lines_in_open_rounds) = (0, 0, 0);
    for round in 0..64 {
        let before = env.lock_lines();
        if round % 2 == 0 {
            // A signs in while B signs out the session that exists.
            let sign_in = {
                let (env, alice) = (env.clone(), alice.clone());
                tokio::spawn(async move { env.sign_in(&alice).await })
            };
            let sign_out = {
                let (env, old) = (env.clone(), previous.clone());
                tokio::spawn(async move { env.sign_out(&old).await.status })
            };
            let (new, status) = (sign_in.await.expect("sign in"), sign_out.await.expect("sign out"));
            assert_eq!(status, 204, "round {round}");
            previous = std::sync::Arc::new(new);
            tokio::time::sleep(Duration::from_millis(40)).await;
            assert_eq!(env.live_sessions(&alice), 1, "round {round}");
            assert_eq!(env.state(&alice), StoreState::Open, "round {round}: a live session and a locked store");
            let lines = env.lock_lines() - before;
            assert!(lines <= 1, "round {round}: {lines} lock lines");
            lines_in_open_rounds += lines;
            ended_open += 1;
        } else {
            // Two sessions end at the same moment: nobody is left.
            let second = std::sync::Arc::new(env.sign_in(&alice).await);
            let before = env.lock_lines();
            let a = {
                let (env, s) = (env.clone(), previous.clone());
                tokio::spawn(async move { env.sign_out(&s).await.status })
            };
            let b = {
                let (env, s) = (env.clone(), second.clone());
                tokio::spawn(async move { env.sign_out(&s).await.status })
            };
            assert_eq!((a.await.expect("a"), b.await.expect("b")), (204, 204));
            let deadline = Instant::now() + PATIENCE;
            while env.state(&alice) != StoreState::Locked {
                assert!(Instant::now() < deadline, "round {round}: nobody is left and the store stays open");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
            assert_eq!(env.live_sessions(&alice), 0, "round {round}");
            assert_eq!(env.lock_lines() - before, 1, "round {round}: one line for two sessions ending together");
            previous = std::sync::Arc::new(env.sign_in(&alice).await);
            assert_eq!(env.state(&alice), StoreState::Open, "round {round}: the sign-in opens it again");
            ended_locked += 1;
        }
    }
    println!(
        "race: 64 rounds, {ended_open} ended with one live session and an open store ({lines_in_open_rounds} lock lines where a lock came first), {ended_locked} with no session and a locked store"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_the_audit_lines_of_a_lock_carry_the_fixed_fields() {
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    env.sign_out(&one).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    let text = env.audit_text();
    let line = text.lines().find(|l| l.contains("secrets.lock")).expect("the lock line");
    let v: serde_json::Value = serde_json::from_str(line).expect("json");
    let mut keys: Vec<&str> = v.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["action", "actor", "addr", "detail", "result", "target", "ts"]);
    assert_eq!(v["detail"], json!({"reason": "signout"}));
    assert_eq!(v["addr"], "");
}

// ---- the store at sign-in -----------------------------------------------------------------------------------------------

use holzwave_server::worker::secretstore::{EncryptedFileStore, SlotKind};

/// The names of the files below a directory, relative to it.
fn names_below(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = unlock_env::files_below(root)
        .iter()
        .map(|p| p.strip_prefix(root).expect("below the root").to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn has_secrets_line(rows: &[unlock_env::Row]) -> bool {
    rows.iter().any(|r| r.action.starts_with("secrets.") || r.action == "recovery.use")
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_after_a_reset_creates_the_store_with_a_password_slot_and_writes_no_secrets_line() {
    let env = UnlockEnv::start().await;
    let (alice, admin, bob, _bob_first) = env.people().await;
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    assert!(env.store_sha256(&bob).is_none(), "the reset left no store");
    let mark = env.audit_lines().len();
    let session = env.sign_in(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Open, "the sign-in made the store and left it open");
    assert_eq!(env.slots_of(&bob), [(SlotKind::Password, [0u8; 16])], "one slot, for the password key");
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["signin.ok/ok"], "creating the store writes no audit line: {rows:?}");
    let mut socket = env.socket(&session).await;
    let profile = ai_profile(&mut socket).await;
    assert_eq!(ai_key_set(&mut socket, &profile, "sk-new-store").await, Ok(()));
    let _ = alice;
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_with_a_passkey_and_prf_after_a_reset_creates_a_prf_slot() {
    let env = UnlockEnv::start().await;
    let (alice, admin, _) = env.bootstrap("alice", PASSWORD).await;
    let (carol, _first, _) = env.invite_with_passkey(&alice, "carol", Some(&prf_text(7))).await;
    assert_eq!(env.slots_of(&carol).iter().map(|s| s.0).collect::<Vec<_>>(), [SlotKind::Prf, SlotKind::Recovery]);
    assert_eq!(env.reset_secrets(&admin, &carol).await.status, 204);
    let mark = env.audit_lines().len();
    let _session = env.sign_in_with_passkey(&carol, Some(&prf_text(7))).await;
    assert_eq!(env.state(&carol), StoreState::Open);
    let slots = env.slots_of(&carol);
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].0, SlotKind::Prf);
    assert_ne!(slots[0].1, [0u8; 16], "a passkey slot is named by its credential");
    assert_eq!(words(&env.audit_since(mark)), ["signin.ok/ok"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_with_no_material_creates_nothing_and_leaves_the_store_locked() {
    // A passkey that sends no PRF output carries the same material as single sign-on: none.
    let env = UnlockEnv::start().await;
    let (alice, admin, _) = env.bootstrap("alice", PASSWORD).await;
    let (carol, _first, _) = env.invite_with_passkey(&alice, "carol", Some(&prf_text(9))).await;
    assert_eq!(env.reset_secrets(&admin, &carol).await.status, 204);
    let mark = env.audit_lines().len();
    let _session = env.sign_in_with_passkey(&carol, None).await;
    assert_eq!(env.state(&carol), StoreState::Locked);
    assert!(env.store_sha256(&carol).is_none(), "nothing was made");
    assert_eq!(words(&env.audit_since(mark)), ["signin.ok/ok"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_never_replaces_a_store_made_with_another_key() {
    let env = UnlockEnv::start().await;
    let (alice, admin, bob, _) = env.people().await;
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    env.restart_worker(&bob).await;
    // A store of somebody else's key sits where the store of bob belongs.
    let other = holzwave_server::unlock::Kek::from_bytes(&[0x5a; 32]);
    let path = env.store_file(&bob);
    EncryptedFileStore::create(&path, SlotKind::Password, [0u8; 16], &other).expect("a foreign store");
    let before = env.store_sha256(&bob).expect("the file");
    let mark = env.audit_lines().len();
    let _session = env.sign_in(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Locked, "the key does not open it");
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()), "not a byte changed");
    assert_eq!(words(&env.audit_since(mark)), ["signin.ok/ok"]);
    let _ = alice;
}

async fn damaged_store_case(region: Region) {
    let env = UnlockEnv::start().await;
    let (_alice, _admin, bob, first) = env.people().await;
    env.sign_out(&first).await;
    env.wait_state(&bob, StoreState::Locked, "the sign-out").await;
    env.restart_worker(&bob).await;
    env.flip_byte(&bob, region);
    let before = env.store_sha256(&bob).expect("the file");
    let mark = env.audit_lines().len();
    let session = env.sign_in(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Locked, "{region:?}: the store stays locked");
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()), "{region:?}: the file is untouched");
    assert_eq!(words(&env.audit_since(mark)), ["signin.ok/ok"], "{region:?}");
    let mut socket = env.socket(&session).await;
    let profile = ai_profile(&mut socket).await;
    assert_eq!(ai_key_set(&mut socket, &profile, "sk-damaged").await, Err("secrets are locked".to_owned()), "{region:?}");
    unlock_env::integration_profile(&mut socket, "prod", &["TOKEN".to_owned()]).await;
    let refused = unlock_env::integration_secret_set(&mut socket, "prod", "TOKEN", "secret-value").await;
    assert_eq!(refused, Err("secrets are locked".to_owned()), "{region:?}");
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()), "{region:?}: the commands wrote nothing");
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_with_a_damaged_header_works_and_leaves_the_store_locked_and_the_file_alone() {
    damaged_store_case(Region::Header).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_with_a_damaged_slot_works_and_leaves_the_store_locked_and_the_file_alone() {
    damaged_store_case(Region::Slot).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_with_a_damaged_body_works_and_leaves_the_store_locked_and_the_file_alone() {
    damaged_store_case(Region::Body).await;
}

// ---- enrolment ----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn enrol_a_password_account_has_its_store_with_the_slots_of_its_keys_and_the_first_session_is_open() {
    let env = UnlockEnv::start().await;
    let mark = env.audit_lines().len();
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    assert_eq!(env.state(&alice), StoreState::Open, "the session starts with the store open");
    let kinds: Vec<SlotKind> = env.slots_of(&alice).iter().map(|s| s.0).collect();
    assert_eq!(kinds, [SlotKind::Password, SlotKind::Recovery]);
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["bootstrap.redeem/ok"], "exactly the line of the enrolment: {rows:?}");
    let mut socket = env.socket(&first).await;
    let profile = ai_profile(&mut socket).await;
    assert_eq!(ai_key_set(&mut socket, &profile, "sk-first").await, Ok(()));
}

#[tokio::test(flavor = "multi_thread")]
async fn enrol_an_invited_member_has_the_same_and_the_audit_holds_the_one_redeem_line() {
    let env = UnlockEnv::start().await;
    let (alice, _first, _) = env.bootstrap("alice", PASSWORD).await;
    let mark = env.audit_lines().len();
    let (bob, _second, _) = env.invite(&alice, "bob", "another horse battery").await;
    assert_eq!(env.state(&bob), StoreState::Open);
    assert_eq!(env.slots_of(&bob).iter().map(|s| s.0).collect::<Vec<_>>(), [SlotKind::Password, SlotKind::Recovery]);
    let rows = env.audit_since(mark);
    assert!(!has_secrets_line(&rows), "{rows:?}");
    assert_eq!(rows.iter().filter(|r| r.action == "account.redeem").count(), 1, "{rows:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn enrol_a_passkey_account_with_prf_has_a_prf_slot_and_a_recovery_slot() {
    let env = UnlockEnv::start().await;
    let (alice, _first, _) = env.bootstrap("alice", PASSWORD).await;
    let mark = env.audit_lines().len();
    let (carol, _session, _) = env.invite_with_passkey(&alice, "carol", Some(&prf_text(3))).await;
    assert_eq!(env.state(&carol), StoreState::Open);
    assert_eq!(env.slots_of(&carol).iter().map(|s| s.0).collect::<Vec<_>>(), [SlotKind::Prf, SlotKind::Recovery]);
    assert!(!has_secrets_line(&env.audit_since(mark)));
}

#[tokio::test(flavor = "multi_thread")]
async fn enrol_a_passkey_account_without_prf_has_only_the_recovery_slot_and_is_open() {
    let env = UnlockEnv::start().await;
    let (alice, _first, _) = env.bootstrap("alice", PASSWORD).await;
    let (carol, _session, _) = env.invite_with_passkey(&alice, "carol", None).await;
    assert_eq!(env.state(&carol), StoreState::Open);
    assert_eq!(env.slots_of(&carol).iter().map(|s| s.0).collect::<Vec<_>>(), [SlotKind::Recovery]);
}

#[tokio::test(flavor = "multi_thread")]
async fn enrol_the_recovery_code_appears_in_exactly_one_answer() {
    let env = UnlockEnv::start().await;
    let (alice, first, finish) = env.bootstrap("alice", PASSWORD).await;
    assert!(finish.text().contains(&alice.recovery), "the answer of the enrolment shows it");
    let mut seen = 0;
    for (method, path) in [("GET", "/api/me"), ("GET", "/api/session"), ("GET", "/api/admin/accounts")] {
        let reply = env.api(&first, method, path, None).await;
        seen += usize::from(reply.text().contains(&alice.recovery));
    }
    assert_eq!(seen, 0, "no later answer repeats it");
    assert!(!env.audit_text().contains(&alice.recovery));
}

// ---- recovery -----------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn recovery_the_code_opens_the_store_after_a_restart_and_is_not_consumed() {
    let env = UnlockEnv::start().await;
    let (_alice, _admin, bob, session) = env.people().await;
    let before = env.store_sha256(&bob).expect("the store");
    env.restart_worker(&bob).await;
    let mark = env.audit_lines().len();
    // The worker is started again by the first control; the person is still signed in and the store is locked.
    env.lock_at_the_worker(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Locked);
    let first = env.unlock_recovery(&session, &bob.recovery).await;
    assert_eq!((first.status, first.json()), (200, json!({"state": "unlocked"})), "{}", first.text());
    assert_eq!(env.state(&bob), StoreState::Open);
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["recovery.use/ok"], "one recovery line and no secrets.unlock line: {rows:?}");
    assert_eq!(rows[0].detail_text("method").as_deref(), Some("recovery_code"));
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()), "using the code changes nothing");

    // Again after another lock: the code was not consumed.
    env.lock_at_the_worker(&bob).await;
    assert_eq!(env.unlock_recovery(&session, &bob.recovery).await.status, 200);
    assert_eq!(env.state(&bob), StoreState::Open);

    // A code with one character changed is refused, writes its line and changes no byte.
    env.lock_at_the_worker(&bob).await;
    let mut wrong: Vec<char> = bob.recovery.chars().collect();
    wrong[0] = if wrong[0] == '0' { '1' } else { '0' };
    let wrong: String = wrong.into_iter().collect();
    let refused = env.unlock_recovery(&session, &wrong).await;
    assert_eq!(refused.status, 401, "{}", refused.text());
    assert_eq!(env.state(&bob), StoreState::Locked);
    assert_eq!(words(&env.audit_since(mark)), ["recovery.use/ok", "recovery.use/ok", "recovery.use/fail"]);
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()));
    println!("recovery: three uses, rows {:?}", words(&env.audit_since(mark)));
}

// ---- the explicit unlock ------------------------------------------------------------------------------------------------

const PASSPHRASE: &str = "a long passphrase for the store";

#[tokio::test(flavor = "multi_thread")]
async fn unlock_a_passphrase_opens_the_store_for_every_session_and_writes_one_line() {
    let env = UnlockEnv::start_tuned(Some(|s| s.recheck = Duration::from_millis(120))).await;
    let (_alice, _admin, bob, one) = env.people().await;
    let two = env.sign_in(&bob).await;
    let set = env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await;
    assert_eq!(set.status, 204, "{}", set.text());
    let mut socket_one = env.socket(&one).await;
    let mut socket_two = env.socket(&two).await;
    env.lock_at_the_worker(&bob).await;
    env.wait_state(&bob, StoreState::Locked, "the lock at the worker").await;
    for socket in [&mut socket_one, &mut socket_two] {
        let locked = socket.event_where("hw-session", Duration::from_secs(5), |p| p["state"] == "locked").await;
        assert!(locked.is_some(), "a socket is told `locked` while the store is locked");
        let again = socket.event_where("hw-session", Duration::from_secs(5), |p| p["state"] == "locked").await;
        assert!(again.is_some(), "and at the next check too");
    }
    let mark = env.audit_lines().len();
    let done = env.unlock_passphrase(&one, PASSPHRASE).await;
    assert_eq!((done.status, done.json()), (200, json!({"state": "unlocked"})), "{}", done.text());
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["secrets.unlock/ok"], "{rows:?}");
    assert_eq!(rows[0].detail_text("method").as_deref(), Some("passphrase"));
    for (name, socket) in [("one", &mut socket_one), ("two", &mut socket_two)] {
        let opened = socket.event_where("hw-session", Duration::from_secs(5), |p| p["state"] == "unlocked").await;
        assert!(opened.is_some(), "socket {name} is told `unlocked`");
    }
    // `unlocked` is said once: the checks that follow say `ok`.
    let (mut unlocked_again, mut checks) = (0, 0);
    let until = Instant::now() + Duration::from_millis(900);
    while Instant::now() < until {
        if let Some(message) = socket_one.read_json(Duration::from_millis(300)).await {
            checks += usize::from(message["event"] == "hw-session");
            unlocked_again += usize::from(message["payload"]["state"] == "unlocked");
        }
    }
    assert!(checks >= 2, "the socket goes on checking its session");
    assert_eq!(unlocked_again, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_a_wrong_passphrase_is_refused_writes_its_line_and_changes_no_byte_and_the_throttle_stops_the_sixth() {
    let env = UnlockEnv::start().await;
    let (_alice, _admin, bob, one) = env.people().await;
    assert_eq!(env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    env.lock_at_the_worker(&bob).await;
    let before = env.store_sha256(&bob).expect("the store");
    let mark = env.audit_lines().len();
    for attempt in 1..=5 {
        let refused = env.unlock_passphrase(&one, "not the passphrase at all").await;
        assert_eq!(refused.status, 401, "attempt {attempt}: {}", refused.text());
    }
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), vec!["secrets.unlock/fail"; 5], "{rows:?}");
    assert_eq!(env.store_sha256(&bob).as_deref(), Some(before.as_str()));
    let sixth = env.unlock_passphrase(&one, "not the passphrase at all").await;
    assert_eq!(sixth.status, 429, "{}", sixth.text());
    assert_eq!(env.audit_since(mark).len(), 5, "the throttled attempt never reached the gate");
    // The right one is throttled too: the lock is on the person, not on the secret.
    assert_eq!(env.unlock_passphrase(&one, PASSPHRASE).await.status, 429);
    assert_eq!(env.audit_since(mark).len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_with_no_passphrase_set_is_refused_and_the_attempt_is_still_one_line() {
    let env = UnlockEnv::start().await;
    let (_alice, _admin, bob, one) = env.people().await;
    env.lock_at_the_worker(&bob).await;
    let mark = env.audit_lines().len();
    assert_eq!(env.unlock_passphrase(&one, PASSPHRASE).await.status, 401);
    assert_eq!(words(&env.audit_since(mark)), ["secrets.unlock/fail"]);
    assert_eq!(env.state(&bob), StoreState::Locked);
}

// ---- the administrator's reset --------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn reset_deletes_the_store_and_its_siblings_and_nothing_else() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, one) = env.people().await;
    let mut socket = env.socket(&one).await;
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string()).await;
    socket.ok("text", &json!({"pane": pane, "text": "RESET=still\r"}).to_string()).await;
    socket.ok("rename_workspace", r#"{"index":0,"name":"Kept Name"}"#).await;
    socket.ok("set_setting", r#"{"key":"font_size","value":17}"#).await;
    assert_eq!(env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    let profile = ai_profile(&mut socket).await;
    ai_key_set(&mut socket, &profile, "sk-before-the-reset").await.expect("a secret");

    let home = env.home(&bob);
    let data = home.join(".holzwave");
    let files_before: Vec<String> = names_below(&home);
    let store_files: Vec<&String> = files_before.iter().filter(|n| n.contains("secrets.store")).collect();
    assert!(!store_files.is_empty(), "the store exists: {files_before:?}");
    let sessions_before = env.ws.test.store.read(|tx| tx.session_list(bob.id)).expect("sessions").len();
    let methods_before = env.ws.test.store.read(|tx| tx.unlock_list(bob.id)).expect("unlock rows").len();
    let mark = env.audit_lines().len();

    let reset = env.reset_secrets(&admin, &bob).await;
    assert_eq!(reset.status, 204, "{}", reset.text());
    assert!(reset.body.is_empty(), "the administrator gets nothing back");
    assert!(env.store_sha256(&bob).is_none());
    let files_after = names_below(&home);
    let expected: Vec<String> = files_before.iter().filter(|n| !n.contains("secrets.store")).cloned().collect();
    assert_eq!(files_after, expected, "only the store and its siblings are gone");
    assert!(data.exists());
    assert_eq!(env.ws.test.store.read(|tx| tx.session_list(bob.id)).expect("sessions").len(), sessions_before);
    let account = env.ws.test.store.account_get(bob.id).expect("read").expect("the account");
    assert_eq!(account.username, "bob");
    let rows_after = env.ws.test.store.read(|tx| tx.unlock_list(bob.id)).expect("unlock rows");
    assert!(rows_after.len() < methods_before, "the rows of the passphrase and the recovery code went with the store");
    assert!(env.ws.test.store.read(|tx| tx.password_get(bob.id)).expect("password").is_some(), "the password stays");

    // The pane keeps running through the reset.
    assert!(env.ws.supervisor.account_stats(bob.key).worker_running);
    socket.ok("text", &json!({"pane": pane, "text": "printf 'hw-%s\\n' $RESET\r"}).to_string()).await;
    assert!(socket.event_where("frame", Duration::from_secs(20), |p| p.to_string().contains("hw-still")).await.is_some());
    // The secrets are gone from the point of view of the commands.
    assert_eq!(ai_has_key(&mut socket, &profile).await, Err("secrets are locked".to_owned()));

    // The layout and the settings are still there after the worker is ended and started again.
    drop(socket);
    env.restart_worker(&bob).await;
    let again = env.sign_in(&bob).await;
    let mut socket = env.socket(&again).await;
    let view = socket.ok("get_layout", "{}").await;
    assert_eq!(view["workspaces"][0]["name"], "Kept Name", "{view}");
    assert!(socket.ok("get_settings", "{}").await.to_string().contains("17"));
    assert_eq!(words(&env.audit_since(mark)).iter().filter(|w| w.starts_with("secrets.reset")).count(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_writes_the_one_line_and_the_old_secrets_open_nothing() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, one) = env.people().await;
    assert_eq!(env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    let mark = env.audit_lines().len();
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    let rows = env.audit_since(mark);
    assert_eq!(words(&rows), ["secrets.reset/ok"], "one line in all, written by the route: {rows:?}");
    assert_eq!((rows[0].actor.as_str(), rows[0].target.as_str()), ("1", bob.id.to_string().as_str()));
    let mark = env.audit_lines().len();
    assert_eq!(env.unlock_passphrase(&one, PASSPHRASE).await.status, 401, "the old passphrase");
    assert_eq!(env.unlock_recovery(&one, &bob.recovery).await.status, 401, "the old recovery code");
    assert_eq!(words(&env.audit_since(mark)), ["secrets.unlock/fail", "recovery.use/fail"]);
    assert_eq!(env.state(&bob), StoreState::Locked);
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_with_a_stopped_worker_works_and_starts_no_shell() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, one) = env.people().await;
    env.sign_out(&one).await;
    env.wait_state(&bob, StoreState::Locked, "the sign-out").await;
    env.restart_worker(&bob).await;
    assert!(!env.ws.supervisor.account_stats(bob.key).worker_running);
    assert!(env.store_sha256(&bob).is_some());
    let reset = env.reset_secrets(&admin, &bob).await;
    assert_eq!(reset.status, 204, "{}", reset.text());
    assert!(env.store_sha256(&bob).is_none());
    assert_eq!(env.ws.supervisor.stats().workers, 0, "no worker is left and no shell was started");
    assert!(!env.ws.supervisor.account_stats(bob.key).worker_running);
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_then_the_next_password_sign_in_makes_a_new_store() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, _one) = env.people().await;
    let old = env.store_sha256(&bob).expect("the store");
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    let session = env.sign_in(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Open);
    let new = env.store_sha256(&bob).expect("a new store");
    assert_ne!(old, new);
    assert_eq!(env.slots_of(&bob), [(SlotKind::Password, [0u8; 16])], "the second truth: one slot, for the password");
    let _ = session;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_racing_an_unlock_ends_with_no_store_and_no_half_store() {
    let env = std::sync::Arc::new(UnlockEnv::start().await);
    let (_alice, admin, _) = env.bootstrap("alice", PASSWORD).await;
    let (bob, one, _) = env.invite(&_alice, "bob", "another horse battery").await;
    let admin = std::sync::Arc::new(admin);
    let one = std::sync::Arc::new(one);
    let bob = std::sync::Arc::new(bob);
    for round in 0..4 {
        env.lock_at_the_worker(&bob).await;
        let reset = {
            let (env, admin, bob) = (env.clone(), admin.clone(), bob.clone());
            tokio::spawn(async move { env.reset_secrets(&admin, &bob).await.status })
        };
        let unlock = {
            let (env, one, bob) = (env.clone(), one.clone(), bob.clone());
            tokio::spawn(async move { env.unlock_recovery(&one, &bob.recovery).await.status })
        };
        assert_eq!(reset.await.expect("reset"), 204, "round {round}");
        let unlocked = unlock.await.expect("unlock");
        assert!(unlocked == 200 || unlocked == 401, "round {round}: {unlocked}");
        assert!(env.store_sha256(&bob).is_none(), "round {round}: the store is gone");
        assert_eq!(env.state(&bob), StoreState::Locked, "round {round}");
        let leftovers: Vec<String> = names_below(&env.home(&bob)).into_iter().filter(|n| n.contains("secrets")).collect();
        assert!(leftovers.is_empty(), "round {round}: {leftovers:?}");
        // A new store for the next round: the person signs in again.
        let again = env.sign_in(&bob).await;
        assert_eq!(env.state(&bob), StoreState::Open);
        drop(again);
        // The recovery row went with the reset; make the next round's code by replacing it.
        let fresh = env.api(&one, "POST", "/api/me/recovery", Some(&json!({}))).await;
        assert_eq!(fresh.status, 200, "{}", fresh.text());
        break;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_a_worker_that_is_gone_when_the_session_ends_writes_no_lock_line() {
    let env = UnlockEnv::start().await;
    let (bob_admin, _admin, _) = env.bootstrap("alice", PASSWORD).await;
    let (bob, one, _) = env.invite(&bob_admin, "bob", "another horse battery").await;
    env.restart_worker(&bob).await;
    assert!(!env.ws.supervisor.account_stats(bob.key).worker_running, "the key left memory with the worker");
    let mark = env.audit_lines().len();
    assert_eq!(env.sign_out(&one).await.status, 204);
    tokio::time::sleep(QUIET).await;
    assert_eq!(words(&env.audit_since(mark)), ["signout/ok"], "nothing to wipe, nothing to say");
    assert!(!env.ws.supervisor.account_stats(bob.key).worker_running, "and the lock started no worker");
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_a_failing_audit_write_changes_nothing_about_the_lock() {
    use std::os::unix::fs::PermissionsExt;
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    // A new day needs a new file, and the directory does not let one be made.
    let dir = env.ws.test.audit_dir.clone();
    env.advance(86_400);
    let writable = std::fs::metadata(&dir).expect("metadata").permissions();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    // The session of the enrolment ran out meanwhile; the sweep locks although the line cannot be written.
    let wiped = env.sweep().await;
    std::fs::set_permissions(&dir, writable).expect("chmod back");
    assert_eq!(wiped, 1, "the lock does not wait for the log");
    assert_eq!(env.state(&alice), StoreState::Locked);
    let _ = one;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unlock_racing_a_lock_ends_in_one_of_the_two_states_and_never_half_open() {
    use holzwave_server::http::routes_unlock::{UnlockGate, UnlockSecret};
    let env = std::sync::Arc::new(UnlockEnv::start().await);
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    assert_eq!(env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    let (mut opened, mut locked) = (0, 0);
    for round in 0..24 {
        // The key is in memory and no session is live any more, but nobody has said so: the sweep wants to lock while the
        // passphrase is typed.
        let _fresh = env.sign_in(&alice).await;
        assert_eq!(env.state(&alice), StoreState::Open, "round {round}");
        env.advance(edge::IDLE + 1);
        let sweep = {
            let env = env.clone();
            tokio::spawn(async move { env.sweep().await })
        };
        let unlock = {
            let (env, id) = (env.clone(), alice.id);
            tokio::spawn(async move { env.gate.unlock_with(id, UnlockSecret::Passphrase(PASSPHRASE)).await })
        };
        let (_wiped, verdict) = (sweep.await.expect("sweep"), unlock.await.expect("unlock"));
        assert!(verdict.is_ok(), "round {round}: the right passphrase is never refused");
        match env.state(&alice) {
            StoreState::Open => opened += 1,
            StoreState::Locked => locked += 1,
        }
        // The file is whole in both cases, with its three slots.
        let whole = EncryptedFileStore::open(&env.store_file(&alice)).expect("a whole store file");
        assert_eq!(whole.slots().expect("slots").len(), 3, "round {round}: password, passphrase and recovery slots");
        // Whatever the end state, the next sweep makes it the one the sessions ask for: no key without a session.
        env.sweep().await;
        assert_eq!(env.state(&alice), StoreState::Locked, "round {round}: no live session, no key");
    }
    println!("unlock racing a lock: 24 rounds, the unlock came last {opened} times and the lock {locked} times");
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_is_refused_to_a_member_and_writes_nothing() {
    let env = UnlockEnv::start().await;
    let (alice, _admin, bob, bob_session) = env.people().await;
    let mark = env.audit_lines().len();
    let before = env.store_sha256(&alice).expect("the store");
    let refused = env.reset_secrets(&bob_session, &alice).await;
    assert!(refused.status == 403 || refused.status == 404, "{}: {}", refused.status, refused.text());
    assert_eq!(env.store_sha256(&alice).as_deref(), Some(before.as_str()));
    assert!(env.audit_since(mark).iter().all(|r| !r.action.starts_with("secrets.")), "{:?}", env.audit_since(mark));
    let _ = bob;
}

/// What a person's secrets are, as words: none of them may be in any audit line.
fn forbidden(user: &User) -> Vec<String> {
    vec![user.password.clone(), user.recovery.clone(), PASSPHRASE.to_owned(), "sk-audit-table".to_owned()]
}

use unlock_env::User;

#[tokio::test(flavor = "multi_thread")]
async fn audit_table_every_case_writes_exactly_its_lines_with_the_fixed_fields_and_no_secret() {
    let env = UnlockEnv::start().await;
    let (alice, admin, _) = env.bootstrap("alice", PASSWORD).await;
    let (bob, one, _) = env.invite(&alice, "bob", "another horse battery").await;
    env.sign_out(&admin).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out of the administrator").await;
    assert_eq!(env.api(&one, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    let mut socket = env.socket(&one).await;
    let profile = ai_profile(&mut socket).await;
    ai_key_set(&mut socket, &profile, "sk-audit-table").await.expect("a secret");
    drop(socket);

    let mut table: Vec<(&str, Vec<String>)> = Vec::new();
    let mut mark = env.audit_lines().len();
    let mut record = |name: &'static str, env: &UnlockEnv, mark: &mut usize| {
        table.push((name, sorted_words(&env.audit_since(*mark))));
        *mark = env.audit_lines().len();
    };

    env.lock_at_the_worker(&bob).await;
    assert_eq!(env.unlock_recovery(&one, &bob.recovery).await.status, 200);
    record("recovery code", &env, &mut mark);
    env.lock_at_the_worker(&bob).await;
    assert_eq!(env.unlock_passphrase(&one, PASSPHRASE).await.status, 200);
    record("passphrase", &env, &mut mark);
    env.lock_at_the_worker(&bob).await;
    assert_eq!(env.unlock_passphrase(&one, "a wrong passphrase").await.status, 401);
    record("wrong passphrase", &env, &mut mark);
    assert_eq!(env.unlock_recovery(&one, &bob.recovery).await.status, 200);
    record("recovery code again", &env, &mut mark);
    let two = env.sign_in(&bob).await;
    record("sign-in with the store open", &env, &mut mark);
    env.sign_out(&one).await;
    tokio::time::sleep(QUIET).await;
    record("sign-out with another session live", &env, &mut mark);
    env.sign_out(&two).await;
    env.wait_state(&bob, StoreState::Locked, "the last sign-out").await;
    record("the last sign-out", &env, &mut mark);
    let three = env.sign_in(&bob).await;
    record("sign-in after the lock", &env, &mut mark);
    env.advance(edge::IDLE + 1);
    env.sweep().await;
    record("the idle time runs out", &env, &mut mark);
    let admin_now = env.sign_in(&alice).await;
    mark = env.audit_lines().len();
    assert_eq!(env.reset_secrets(&admin_now, &bob).await.status, 204);
    record("the reset", &env, &mut mark);
    let four = env.sign_in(&bob).await;
    record("sign-in after the reset", &env, &mut mark);
    let _ = (three, four);

    let expected: Vec<(&str, Vec<&str>)> = vec![
        ("recovery code", vec!["recovery.use/ok"]),
        ("passphrase", vec!["secrets.unlock/ok"]),
        ("wrong passphrase", vec!["secrets.unlock/fail"]),
        ("recovery code again", vec!["recovery.use/ok"]),
        ("sign-in with the store open", vec!["signin.ok/ok"]),
        ("sign-out with another session live", vec!["signout/ok"]),
        ("the last sign-out", vec!["secrets.lock/ok", "signout/ok"]),
        ("sign-in after the lock", vec!["signin.ok/ok"]),
        ("the idle time runs out", vec!["secrets.lock/ok"]),
        ("the reset", vec!["secrets.reset/ok"]),
        ("sign-in after the reset", vec!["signin.ok/ok"]),
    ];
    for ((name, got), (want_name, want)) in table.iter().zip(&expected) {
        assert_eq!(name, want_name);
        assert_eq!(got, &want.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>(), "case `{name}`");
    }
    assert_eq!(table.len(), expected.len());

    // Every line has the fixed fields, and none holds a secret.
    let text = env.audit_text();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).expect("json");
        let mut keys: Vec<&str> = v.as_object().expect("object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["action", "actor", "addr", "detail", "result", "target", "ts"], "{line}");
    }
    for secret in forbidden(&bob).into_iter().chain(forbidden(&alice)) {
        assert!(!text.contains(&secret), "a secret is in the audit log");
    }
    println!("audit table: {} cases, {} lines", expected.len(), text.lines().count());
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_on_an_open_store_changes_no_byte_and_writes_no_secrets_line() {
    let env = UnlockEnv::start().await;
    let (alice, _first, _) = env.bootstrap("alice", PASSWORD).await;
    let before = env.store_sha256(&alice).expect("the store");
    let mark = env.audit_lines().len();
    let _second = env.sign_in(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Open);
    assert_eq!(env.store_sha256(&alice).as_deref(), Some(before.as_str()));
    assert_eq!(words(&env.audit_since(mark)), ["signin.ok/ok"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn signin_after_a_password_change_opens_the_store_with_the_new_password() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    let body = json!({"current": PASSWORD, "new": "a brand new password here"});
    let changed = env.api(&first, "POST", "/api/me/password", Some(&body)).await;
    assert_eq!(changed.status, 204, "{}", changed.text());
    env.sign_out(&first).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    let mut renewed = alice.clone();
    renewed.password = "a brand new password here".to_owned();
    let _session = env.sign_in(&renewed).await;
    assert_eq!(env.state(&alice), StoreState::Open, "the new password key opens the store");
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_a_new_code_replaces_the_old_one_which_then_opens_nothing() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    let fresh = env.api(&first, "POST", "/api/me/recovery", Some(&json!({}))).await;
    assert_eq!(fresh.status, 200, "{}", fresh.text());
    let new_code = fresh.json()["recovery_code"].as_str().expect("a code").to_owned();
    assert_ne!(new_code, alice.recovery);
    env.lock_at_the_worker(&alice).await;
    assert_eq!(env.unlock_recovery(&first, &alice.recovery).await.status, 401, "the old code");
    assert_eq!(env.state(&alice), StoreState::Locked);
    assert_eq!(env.unlock_recovery(&first, &new_code).await.status, 200, "the new code");
    assert_eq!(env.state(&alice), StoreState::Open);
    assert!(!env.audit_text().contains(&new_code));
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_a_recovery_code_typed_as_a_passphrase_opens_nothing() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    assert_eq!(env.api(&first, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    env.lock_at_the_worker(&alice).await;
    assert_eq!(env.unlock_passphrase(&first, &alice.recovery).await.status, 401);
    assert_eq!(env.unlock_recovery(&first, PASSPHRASE).await.status, 401);
    assert_eq!(env.state(&alice), StoreState::Locked);
}

#[tokio::test(flavor = "multi_thread")]
async fn unlock_the_passphrase_opens_the_store_after_the_worker_was_restarted() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    assert_eq!(env.api(&first, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": PASSPHRASE}))).await.status, 204);
    env.restart_worker(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Locked, "a worker that starts again finds the store locked");
    let mark = env.audit_lines().len();
    assert_eq!(env.unlock_passphrase(&first, PASSPHRASE).await.status, 200);
    assert_eq!(env.state(&alice), StoreState::Open);
    assert_eq!(words(&env.audit_since(mark)), ["secrets.unlock/ok"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_twice_is_harmless_and_each_call_has_its_one_line() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, _) = env.people().await;
    let mark = env.audit_lines().len();
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    assert_eq!(words(&env.audit_since(mark)), ["secrets.reset/ok", "secrets.reset/ok"]);
    assert!(env.store_sha256(&bob).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn last_session_a_person_who_signs_in_again_before_the_old_session_ends_is_never_locked() {
    let env = UnlockEnv::start().await;
    let (alice, one, _) = env.bootstrap("alice", PASSWORD).await;
    let before = env.lock_lines();
    // The browser still holds the old cookie while it signs in again: the old session ends, the new one lives.
    let two = env.sign_in(&alice).await;
    assert_eq!(env.sign_out(&one).await.status, 204);
    tokio::time::sleep(QUIET).await;
    assert_eq!(env.state(&alice), StoreState::Open);
    assert_eq!(env.lock_lines(), before);
    assert_eq!(env.live_sessions(&alice), 1);
    let _ = two;
}
