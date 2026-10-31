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
use unlock_env::{UnlockEnv, ai_has_key, ai_key_set, ai_profile};
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
    let words: Vec<String> = rows.iter().map(|r| r.word()).collect();
    assert_eq!(words, ["signout/ok", "secrets.lock/ok"], "{rows:?}");
    assert_eq!(rows[1].detail_text("reason").as_deref(), Some("signout"));
    assert_eq!((rows[1].actor.as_str(), rows[1].target.as_str()), (alice.id.to_string().as_str(), alice.id.to_string().as_str()));

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
